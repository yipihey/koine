//! The browser binary: `dx serve`, `dx build --platform web`. Everything is in
//! the library (`src/lib.rs`), so a host-linked app can launch the same page
//! (`docs/plan-koine-mobile.md` M-D1, M-D2).

fn main() {
    koine_web::launch();
}
