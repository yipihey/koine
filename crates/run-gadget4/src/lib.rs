//! GADGET-4 adapter for the run convention (`docs/plan-run-convention.md`,
//! R-D9.1): the schema harvested from the code's own startup echo, a renderer
//! for its `param.txt`, a launcher that writes a conforming run directory, and
//! an adopter for runs GADGET-4 already did.

pub mod adopt;
pub mod echo;
pub mod error;
pub mod exitmap;
pub mod h5;
pub mod info;
pub mod launch;
pub mod post;
pub mod render;
pub mod rundir;
pub mod schema;
pub mod table;
pub mod value;

pub use adopt::{adopt, AdoptOptions};
pub use error::{Error, Problem};
pub use launch::{launch, LaunchOptions, Outcome};
