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
    #[cfg(target_os = "macos")]
    let menu = macos::menu_bar(title).map_err(|e| say_why_no_menu(&e)).ok();
    event_loop.run_return(|event, _, flow| {
        *flow = ControlFlow::Wait;
        #[cfg(target_os = "macos")]
        while let Ok(chosen) = muda::MenuEvent::receiver().try_recv() {
            if menu
                .as_ref()
                .is_some_and(|(_, reload)| chosen.id == *reload)
            {
                let _ = _webview.reload();
            }
        }
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

/// A window without a menu bar still works; say why it has none.
#[cfg(target_os = "macos")]
fn say_why_no_menu(error: &dyn std::fmt::Display) {
    eprintln!("koine-window: no menu bar ({error}); Cmd-C/V and Cmd-Q will not work");
}

/// The macOS menu bar. A WebView takes Cut, Copy, Paste and Select All from
/// the Edit menu's selectors, so without one those keys do nothing in a
/// text field; Quit is the only way to Cmd-Q. View → Reload is the one item
/// handled here, by id.
#[cfg(target_os = "macos")]
mod macos {
    use muda::accelerator::{Accelerator, Code, Modifiers};
    use muda::{Menu, MenuId, MenuItem, PredefinedMenuItem as P, Submenu};

    /// Install the menu bar; the menu must be kept alive while it shows.
    pub(super) fn menu_bar(title: &str) -> Result<(Menu, MenuId), muda::Error> {
        let reload = MenuItem::new(
            "Reload",
            true,
            Some(Accelerator::new(Some(Modifiers::SUPER), Code::KeyR)),
        );
        let id = reload.id().clone();
        let app = Submenu::with_items(
            title,
            true,
            &[
                &P::about(None, None),
                &P::separator(),
                &P::hide(None),
                &P::hide_others(None),
                &P::show_all(None),
                &P::separator(),
                &P::quit(None),
            ],
        )?;
        let edit = Submenu::with_items(
            "Edit",
            true,
            &[
                &P::undo(None),
                &P::redo(None),
                &P::separator(),
                &P::cut(None),
                &P::copy(None),
                &P::paste(None),
                &P::select_all(None),
            ],
        )?;
        let view = Submenu::with_items("View", true, &[&reload, &P::fullscreen(None)])?;
        let window = Submenu::with_items(
            "Window",
            true,
            &[&P::minimize(None), &P::close_window(None)],
        )?;
        let menu = Menu::new();
        for sub in [&app, &edit, &view, &window] {
            menu.append(sub)?;
        }
        menu.init_for_nsapp();
        window.set_as_windows_menu_for_nsapp();
        Ok((menu, id))
    }
}
