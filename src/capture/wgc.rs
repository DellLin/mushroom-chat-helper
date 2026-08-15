//! Windows.Graphics.Capture 擷取層。
//!
//! 執行緒模型:本模組 spawn 一條指令執行緒,收到 Start 後建立
//! free-threaded FramePool;FrameArrived 回呼中複製到 staging texture、
//! 讀回 CPU、以 bounded channel 丟給影像管線(滿了就丟幀)。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};
use windows::core::{IInspectable, Interface};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE,
    D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

use crate::model::{CaptureCmd, CaptureState, FramePacket, UiEvent};

struct D3D {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    winrt_device: IDirect3DDevice,
}

fn init_d3d() -> anyhow::Result<D3D> {
    unsafe {
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        let mut result = D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            windows::Win32::Foundation::HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        );
        if result.is_err() {
            // 硬體裝置失敗時退回 WARP 軟體算繪。
            result = D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_WARP,
                windows::Win32::Foundation::HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            );
        }
        result?;
        let device = device.ok_or_else(|| anyhow::anyhow!("無法建立 D3D11 裝置"))?;
        let context = context.ok_or_else(|| anyhow::anyhow!("無法取得 D3D11 context"))?;
        let dxgi: IDXGIDevice = device.cast()?;
        let inspectable = CreateDirect3D11DeviceFromDXGIDevice(&dxgi)?;
        let winrt_device: IDirect3DDevice = inspectable.cast()?;
        Ok(D3D { device, context, winrt_device })
    }
}

/// FrameArrived 回呼共享的狀態。
struct HandlerCtx {
    device: ID3D11Device,
    winrt_device: IDirect3DDevice,
    context: Mutex<ID3D11DeviceContext>,
    /// (staging, w, h)
    staging: Mutex<Option<(ID3D11Texture2D, u32, u32)>>,
    frame_tx: Sender<FramePacket>,
    last_sent: Mutex<Instant>,
    interval_ms: Arc<AtomicU64>,
}

// SAFETY: HandlerCtx 只在 free-threaded 環境下使用——擷取執行緒以
// RO_INIT_MULTITHREADED 初始化,且 FramePool 由 CreateFreeThreaded 建立,
// 允許 FrameArrived 回呼在任意執行緒觸發並存取這些 COM 介面。
unsafe impl Send for HandlerCtx {}
unsafe impl Sync for HandlerCtx {}

struct ActiveCapture {
    session: GraphicsCaptureSession,
    pool: Direct3D11CaptureFramePool,
}

impl ActiveCapture {
    fn stop(self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

fn start_capture(
    d3d: &D3D,
    hwnd: isize,
    frame_tx: Sender<FramePacket>,
    interval_ms: Arc<AtomicU64>,
) -> anyhow::Result<ActiveCapture> {
    if !GraphicsCaptureSession::IsSupported()? {
        anyhow::bail!("此系統不支援 Windows.Graphics.Capture(需 Windows 10 1903 以上)");
    }
    let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
    let item: GraphicsCaptureItem =
        unsafe { interop.CreateForWindow(HWND(hwnd as *mut core::ffi::c_void))? };
    let size = item.Size()?;
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &d3d.winrt_device,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        2,
        size,
    )?;
    let session = pool.CreateCaptureSession(&item)?;
    // 不擷取滑鼠游標;移除黃色擷取邊框(舊版系統不支援就忽略)。
    let _ = session.SetIsCursorCaptureEnabled(false);
    let _ = session.SetIsBorderRequired(false);

    let ctx = Arc::new(HandlerCtx {
        device: d3d.device.clone(),
        winrt_device: d3d.winrt_device.clone(),
        context: Mutex::new(d3d.context.clone()),
        staging: Mutex::new(None),
        frame_tx,
        last_sent: Mutex::new(Instant::now() - std::time::Duration::from_secs(1)),
        interval_ms,
    });

    pool.FrameArrived(&TypedEventHandler::<
        Direct3D11CaptureFramePool,
        IInspectable,
    >::new(move |pool, _| {
        if let Some(pool) = pool.as_ref() {
            if let Err(e) = on_frame(&ctx, pool) {
                log::debug!("frame 處理失敗: {e}");
            }
        }
        Ok(())
    }))?;

    session.StartCapture()?;
    Ok(ActiveCapture { session, pool })
}

fn on_frame(ctx: &HandlerCtx, pool: &Direct3D11CaptureFramePool) -> anyhow::Result<()> {
    // 一定要先取走 frame,避免 pool 積壓。
    let frame = pool.TryGetNextFrame()?;

    // 節流:未到間隔就丟棄這幀。
    {
        let mut last = ctx.last_sent.lock().unwrap();
        let interval =
            std::time::Duration::from_millis(ctx.interval_ms.load(Ordering::Relaxed).max(30));
        if last.elapsed() < interval {
            return Ok(());
        }
        *last = Instant::now();
    }

    let surface = frame.Surface()?;
    let access: IDirect3DDxgiInterfaceAccess = surface.cast()?;
    let tex: ID3D11Texture2D = unsafe { access.GetInterface::<ID3D11Texture2D>()? };
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { tex.GetDesc(&mut desc) };
    let (w, h) = (desc.Width, desc.Height);
    if w == 0 || h == 0 {
        return Ok(());
    }

    // 視窗尺寸變動 → 重建 pool(下一幀生效,本幀照舊處理)。
    if let Ok(cs) = frame.ContentSize() {
        if cs.Width > 0 && cs.Height > 0 && (cs.Width as u32 != w || cs.Height as u32 != h) {
            let _ = pool.Recreate(
                &ctx.winrt_device,
                DirectXPixelFormat::B8G8R8A8UIntNormalized,
                2,
                SizeInt32 { Width: cs.Width, Height: cs.Height },
            );
        }
    }

    // 準備/重建 staging texture。
    {
        let mut staging_guard = ctx.staging.lock().unwrap();
        let need_new = match staging_guard.as_ref() {
            Some((_, sw, sh)) => *sw != w || *sh != h,
            None => true,
        };
        if need_new {
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: desc.Format,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut staging: Option<ID3D11Texture2D> = None;
            unsafe { ctx.device.CreateTexture2D(&staging_desc, None, Some(&mut staging))? };
            *staging_guard =
                Some((staging.ok_or_else(|| anyhow::anyhow!("staging 建立失敗"))?, w, h));
        }
    }

    let staging_guard = ctx.staging.lock().unwrap();
    let (staging, _, _) = staging_guard.as_ref().unwrap();
    let context = ctx.context.lock().unwrap();

    let mut bgra = vec![0u8; (w * h * 4) as usize];
    unsafe {
        context.CopyResource(staging, &tex);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        let src = mapped.pData as *const u8;
        let row_pitch = mapped.RowPitch as usize;
        let row_bytes = (w * 4) as usize;
        for y in 0..h as usize {
            std::ptr::copy_nonoverlapping(
                src.add(y * row_pitch),
                bgra.as_mut_ptr().add(y * row_bytes),
                row_bytes,
            );
        }
        context.Unmap(staging, 0);
    }
    drop(context);
    drop(staging_guard);

    // 滿了就丟,讓管線永遠處理最新畫面。
    let _ = ctx.frame_tx.try_send(FramePacket { width: w, height: h, bgra });
    Ok(())
}

/// 擷取執行緒主迴圈。
pub fn spawn(
    cmd_rx: Receiver<CaptureCmd>,
    frame_tx: Sender<FramePacket>,
    ui_tx: Sender<UiEvent>,
    interval_ms: Arc<AtomicU64>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("capture".into())
        .spawn(move || {
            unsafe {
                let _ = RoInitialize(RO_INIT_MULTITHREADED);
            }
            let mut d3d: Option<D3D> = None;
            let mut active: Option<ActiveCapture> = None;

            while let Ok(cmd) = cmd_rx.recv() {
                match cmd {
                    CaptureCmd::Start { hwnd, title } => {
                        if let Some(a) = active.take() {
                            a.stop();
                        }
                        if d3d.is_none() {
                            match init_d3d() {
                                Ok(d) => d3d = Some(d),
                                Err(e) => {
                                    let _ = ui_tx.send(UiEvent::CaptureState(
                                        CaptureState::Failed(format!("D3D11 初始化失敗: {e}")),
                                    ));
                                    continue;
                                }
                            }
                        }
                        match start_capture(
                            d3d.as_ref().unwrap(),
                            hwnd,
                            frame_tx.clone(),
                            interval_ms.clone(),
                        ) {
                            Ok(a) => {
                                active = Some(a);
                                let _ = ui_tx
                                    .send(UiEvent::CaptureState(CaptureState::Running(title)));
                            }
                            Err(e) => {
                                let _ = ui_tx.send(UiEvent::CaptureState(CaptureState::Failed(
                                    format!("開始擷取失敗: {e}"),
                                )));
                            }
                        }
                    }
                    CaptureCmd::Stop => {
                        if let Some(a) = active.take() {
                            a.stop();
                        }
                        let _ = ui_tx.send(UiEvent::CaptureState(CaptureState::Idle));
                    }
                    CaptureCmd::Shutdown => break,
                }
            }
            if let Some(a) = active.take() {
                a.stop();
            }
        })
        .expect("無法建立擷取執行緒")
}
