---
name: regex-recipes
description: Ready-made regex-lab patterns for identifiers researchers meet (DOI, arXiv id, ISO date, semver, email, URL, IPv4, UUID) and for log lines. Use before writing one of these patterns by hand.
---

# Regex recipes

Before writing a pattern for one of these, look in the library
(`regex-lab-service_patterns`). If it is not there, the **Regex recipes** page
saves them all (`regex-lab-service_save-pattern`, one call each); then pass the
name as `saved`:

| name | matches | groups |
|---|---|---|
| `doi` | A DOI (digital object identifier) (`10.1103/PhysRevLett.116.061102`) | none |
| `arxiv-id` | A new-style arXiv identifier, with its version (`2310.06825v2`) | yymm, number, version |
| `iso-date` | An ISO 8601 calendar date (`2026-10-17`) | year, month, day |
| `semver` | A semantic version (`v1.13.1-rc.1`) | major, minor, patch, pre, build |
| `email` | An email address (the common shape, not all of RFC 5322) (`ada@example.org`) | user, domain |
| `url` | An http or https URL, split into host, port, path and query (`https://example.org:8080/a/b?x=1`) | host, port, path, query |
| `ipv4` | An IPv4 address (`192.168.1.20`) | none |
| `uuid` | A UUID, any case (`6F9619FF-8B86-D011-B42D-00C04FC964FF`) | none |
| `log-line` | A tracing-style log line: time, level, target, message (`2026-10-17T23:20:17Z  INFO koine-host: kit installed`) | time, level, target, message |

`log-line` needs the `m` flag (it is saved with it): `^` and `$` are then each
line's. The **Log fields** page shows `captures-all`'s `records` for it as a
table, one row per log line.
