# heycode-web

Provider-independent public web search/fetch Service Definition, effect-owned
provider registry and portable provider plugins. Model-facing tools live in
`heycode-tools` and consume this service; they never construct HTTP clients or
read provider credentials directly.

`web` publishes `WebRegistry`; providers register validated descriptors as
Context effects. Each operation uses its configured provider id or auto-selects
only when exactly one locally available provider supports it; multiple usable
providers fail as ambiguous instead of depending on registration order. Requests/results
bound queries, counts, HTTP(S) URLs, bytes and text; errors and Debug retain no
query, URL, response body, DNS/OS text or credential. The caller's cancellation
token owns each operation, provider output is revalidated, and shutdown is
terminal even through a held registry. The shared public-URL value boundary
rejects obvious loopback, link-local, private, metadata and reserved-local hosts
before any provider can publish or dispatch them. Named-host DNS admission
remains a transport responsibility because only the connecting provider can
bind validation to the socket it actually opens.

`web-portable` contributes search and fetch. Search resolves `BRAVE_API_KEY` at
operation time or uses keyless DuckDuckGo Lite, normalizing both to one result
shape; provider search redirects are refused so custom credential headers never
cross an unreviewed origin. Fetch streams to cap+1, strips HTML, rejects typed
private IPv4/IPv6 literals and any mixed/private DNS answer, pins the admitted
addresses into the connection, and manually re-admits every relative or absolute
redirect. Loops, unsafe schemes/userinfo and more than five hops fail before the
next request. The pinned fetch client disables reqwest's ambient proxy support;
a proxy would delegate hostname resolution and connection away from the address
set the guard admitted. DNS cancellation owns no detachable task handle.

`web-policy` owns live Settings namespace `web`. Search and fetch select
providers independently and each has bounded `allow`/`block` domain rules. A
base domain matches itself and subdomains; block wins. Search results are
filtered before publication, while fetch rules travel into every provider hop
and the registry rechecks the final URL. `heycode-status` contributes optional
plugin `status-web` and `/web`, which renders provider availability, effective
selection and the bounded policy without exposing queries or fetched URLs.
When block rules exist, public IP literals are refused because they may be the
resolved form of a blocked name, and an IDN host requires a matching IDN allow
rule because its punycoded label cannot be compared safely with an arbitrary
ASCII homograph rule. A broad ASCII parent allow rule is not sufficient. With
no block rules, ordinary public literals and IDNs retain their prior behavior.

WEB03 adds optional default `web-extract`, exact `web_processor:portable-readable`
and a weak provider→processor handle. Portable fetch separates a 4 MiB raw cap
from the readable output cap. HTML is rendered to bounded text with links; PDF
uses lopdf 0.44 bounded load/per-page extraction, a 256-page ceiling, page
markers, cancellation and a joined 10-second worker. Extraction then admits the
raw source through `attachments`, so `attachment/added` durably retains final
URL, title, retrieval time, raw-truncation state, page count and content address.
`web_fetch` renders an escaped citeable source/page count and never exposes the
hash. Unsupported binary is not lossy-decoded.

WEB05 keeps retrieved text explicitly non-authoritative after it leaves this
crate. Both model-facing Web tools declare core `UntrustedContentBoundary::web`
through the Tool trait. The guarded pipeline carries that classification into
the durable session result; Agent and TUI never infer it from `web_*` names or
parse the rendered citation. The marker does not alter web provider selection
or grant any approval capability.

ATT03 reuses that parser without inventing web provenance. `web-extract` also
publishes effect-owned service `document-extractor`; its local input accepts
content-verified PDF/HTML/plain text up to 32 MiB and returns bounded UTF-8,
title/page count and truncation only. It shares the same lopdf/html2text limits,
10-second deadline, cancellation and always-joined worker. It never writes an
attachment event itself. `agent-documents` owns the later derived-text commit
and durable native-versus-extracted route. Held extractors return Stopped after
Context shutdown.

## Verification

```sh
cargo fmt --all --check
cargo clippy -p heycode-web --all-targets -- -D warnings
cargo test -p heycode-web
```
