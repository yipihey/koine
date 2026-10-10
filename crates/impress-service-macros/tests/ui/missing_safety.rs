use impress_service_macros::impress_service_impl;

impress_service_impl! {
    service = UndeclaredSafety,
    instance = || (),
    since = "0.1.0",
    effects = {},
    methods = [],
}
