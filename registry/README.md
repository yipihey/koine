# The example package registry

A koine package registry is `index.json` beside the bundles it lists
(docs/plan-koine-research-os.md P-D5..P-D8). This one has one package,
`run-compare`, whose source is `src/run-compare/`.

Add it to a host:

```bash
koine-host --registry ./registry            # at start
# or, on a running host: package-service_registry-add {"url": "<path or URL>"}
```

then `package-service_search`, `package-service_fetch {"name": "run-compare"}`
(the review) and `package-service_install {"path", "digest"}` from the
review, or the Packages surface.

Publish a change to a package (the bundle and the index entry are rewritten;
`cargo test -p koine-packages` fails until you do):

```bash
cargo run -p koine-host -- package bundle registry/src/run-compare \
    --out registry --index registry/index.json --registry-name koine-examples
```

Served over HTTP (any static file server), the same directory is a registry
at `https://<host>/<path>/`.
