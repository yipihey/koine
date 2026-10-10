use impress_service_macros::impress_service_impl;

impress_service_impl! {
    service = UndeclaredSince,
    instance = || (),
    safety = read_only,
    effects = {},
    methods = [],
}
