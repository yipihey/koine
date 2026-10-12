//! The native window: a tao event loop and one wry WebView on the page.

use crate::{Opened, WindowError};
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    platform::run_return::EventLoopExtRunReturn,
    window::WindowBuilder,
};
use wry::WebViewBuilder;

pub(crate) fn run(url: &str, title: &str) -> Result<Opened, WindowError> {
    let err = |e: &dyn std::fmt::Display| WindowError::WebView(e.to_string());
    let mut event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title(title)
        .with_inner_size(LogicalSize::new(1100.0, 760.0))
        .build(&event_loop)
        .map_err(|e| err(&e))?;
    let builder = WebViewBuilder::new().with_url(url);
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let _webview = builder.build(&window).map_err(|e| err(&e))?;
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let _webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window
            .default_vbox()
            .ok_or_else(|| err(&"no gtk container"))?;
        builder.build_gtk(vbox).map_err(|e| err(&e))?
    };
    event_loop.run_return(|event, _, flow| {
        *flow = ControlFlow::Wait;
        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            *flow = ControlFlow::Exit;
        }
    });
    Ok(Opened::WebView)
}
