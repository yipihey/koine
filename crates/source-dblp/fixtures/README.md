# DBLP fixtures (S-D1: the conformance fixture per adapter)

**dblp.org could not be recorded.** On 2026-10-19 every request to
`https://dblp.org/search/publ/api`, `https://dblp.org/rec/…` and
`https://dblp.org/` itself, from a plain `curl` user-agent, from this
adapter's user-agent (`koine-source-dblp/0.1 (mailto:tabel@stanford.edu)`),
and from the mirrors `dblp.uni-trier.de` and `dblp.dagstuhl.de`, answered
`HTTP/2 202` with an Anubis proof-of-work page and no data (`robots.txt` alone
answered 200). Passing such a check is bot-detection bypass, which this work
does not do, so:

| File | What it is |
|---|---|
| `bot-challenge.html` / `bot-challenge.headers` | **Recorded.** `curl -sS -D bot-challenge.headers -A 'koine-source-dblp/0.1 (mailto:tabel@stanford.edu)' 'https://dblp.org/search/publ/api?q=attention%20is%20all%20you%20need&format=json&h=3&f=0' -o bot-challenge.html`, 2026-10-19 23:23 UTC. Two edits: the `set-cookie` headers are dropped and the client address in the page's challenge metadata is replaced by `REDACTED`. The adapter tests that this is reported as an `error` diagnostic that says so. |
| `search.json` | **Hand-written, not recorded.** The shape of `search/publ/api?format=json` as DBLP documents it: `result.hits{@total,@computed,@sent,@first,hit[]}`, each `hit.info` with `authors.author` (a list, or a single object when there is one author), `title` with its trailing full stop, `venue` (a string, or an array), `pages`, `year`, `type`, `key`, `ee` (a string, or an array), an optional `doi`. Three hits chosen to exercise those variants: a conference paper (the Transformer, `conf/nips/VaswaniSPUJGKP17`), a journal article with one author and a homonym number (`David M. Blei 0001`), and a CoRR record with `venue` as an array, an `abs/…` volume and two `ee`. The `@id`s are invented. |
| `search-empty.json` | **Hand-written.** A result with `@total` 0 and no `hit` key. |
| `rec.xml`, `rec-article.xml` | **Hand-written.** `rec/<key>.xml` as DBLP serves it: `<dblp><inproceedings key=…>` / `<article key=…>` with `author`, `title`, `pages`, `year`, `booktitle`/`journal`/`volume`, repeated `ee`, `url`. |
| `rec.bib` | **Hand-written.** `rec/<key>.bib` in DBLP's format (`DBLP:<key>` citation key, `biburl`, `bibsource`). |

None of the hand-written files is evidence about DBLP's current output. When
dblp.org answers again, the commands to replace them with recordings are:

```
curl -sS -D search.headers -A 'koine-source-dblp/0.1 (mailto:tabel@stanford.edu)' \
  'https://dblp.org/search/publ/api?q=Vaswani%20attention%20is%20all&format=json&h=3&f=0' -o search.json
curl -sS -D rec.xml.headers -A '…' 'https://dblp.org/rec/conf/nips/VaswaniSPUJGKP17.xml' -o rec.xml
curl -sS -D rec.bib.headers -A '…' 'https://dblp.org/rec/conf/nips/VaswaniSPUJGKP17.bib' -o rec.bib
```

and `cargo test -p source-dblp -- --ignored live` is the live check; the
tests that read these files will say what, if anything, differs.
