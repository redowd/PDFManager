use std::path::Path;

use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;
use wry::{WebContext, WebViewBuilder};

/// 桌面窗口：用系统 WebView2 承载内嵌前端，主线程运行事件循环。
/// 内嵌服务仍在后台监听本机端口，页面通过相对路径访问 API。
pub fn run(url: &str, data_dir: &Path) -> ! {
    let _ = std::fs::create_dir_all(data_dir);

    let event_loop = EventLoop::new();

    // 窗口图标：与 exe 资源图标同源（生成脚本 tools/make_icon.py）
    let icon = tao::window::Icon::from_rgba(
        include_bytes!("../assets/icon_32.rgba").to_vec(),
        32,
        32,
    )
    .ok();

    let mut builder = WindowBuilder::new()
        .with_title("PDF 文件管理系统")
        .with_inner_size(LogicalSize::new(1440.0, 900.0))
        .with_min_inner_size(LogicalSize::new(960.0, 600.0));
    if let Some(icon) = icon {
        builder = builder.with_window_icon(Some(icon));
    }
    let window = builder.build(&event_loop).expect("创建应用窗口失败");

    let mut web_context = WebContext::new(Some(data_dir.to_path_buf()));
    let _webview = WebViewBuilder::new_with_web_context(&mut web_context)
        .with_url(url)
        .build(&window)
        .expect("创建 WebView 失败");

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            *control_flow = ControlFlow::Exit;
        }
    })
}
