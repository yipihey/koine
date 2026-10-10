fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    impress_schema_ref_codegen::generate();
}
