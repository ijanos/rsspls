# Customized fork of rsspls
## TODO

- [x] switch to GitHub Actions
- [x] bump to latest dependencies / latest rust
- [x] add update hooks
- [x] switch to `scraper` for better selector support
- [x] create a container to easier deploys
- [x] try to match selector on the root item first (rsspls can only match on children)
- [x] use default link selector `a[href], area[href]`
- [x] make feeds with zero items error out (configurable)
- [x] Support parsing JSON endpoints with jaq filters
- [ ] combine multiple sources into one feed


## Container build

```bash
podman build -f Containerfile -t rsspls .
podman run --rm rsspls

# use the latest published version
podman run --rm -v /some/dir:/rsspls ghcr.io/ijanos/rsspls:latest
```

The container image sets `RSSPLS_HOME=/rsspls` by default. When `RSSPLS_HOME` is set,
`rsspls` reads `feeds.toml` from `$RSSPLS_HOME/feeds.toml` and stores cache files in
`$RSSPLS_HOME/cache`.

The container has `rsync` and `rclone` installed for post update hooks. 

---
---

<h1 align="center">
  <img src="feed-icon.svg" width="48" alt=""><br>
  RSS Please
</h1>

<div align="center">
  <strong>A small tool (<code>rsspls</code>) to generate RSS feeds from web
  pages that lack them. It runs on BSD, Linux, macOS, Windows, and
  more.</strong>
</div>

<br>

<div align="center">
  <a href="https://cirrus-ci.com/github/wezm/rsspls">
    <img src="https://api.cirrus-ci.com/github/wezm/rsspls.svg" alt="Build Status"></a>
  <a href="https://crates.io/crates/rsspls">
    <img src="https://img.shields.io/crates/v/rsspls.svg" alt="Version">
  </a>
  <img src="https://img.shields.io/crates/l/rsspls.svg" alt="License">
</div>

<br>

`rsspls` generates RSS feeds from web pages. Example use cases:

* Create a feed for a blog that does not have one so that you will know when
  there are new posts.
* Create a feed from the search results on real estate agent's website so that
  you know when there are new listings—without having to check manually all the
  time.
* Create a feed of the upcoming tour dates of your favourite band or DJ.
* Create a feed of the product page for a company, so you know when new
  products are added.

The idea is that you will then subscribe to the generated feeds in your feed
reader. This will typically require the feeds to be hosted via a web server.

For more information including installation instructions, documentation, and
news visit the [RSS Please website][website].

<div align="center">
  <a href="https://rsspls.7bit.org/"><img src="visit-website.png" width="198" alt="Visit Website"></a>
</div>

Per-feed minimum item threshold
-------------------------------

You can require a feed to produce at least a certain number of successfully created items.
Set `min_items` inside `[feed.config]`:

```rsspls/README.md#L1-8
[[feed]]
title = "Example"
filename = "example.xml"

[feed.config]
url = "https://example.com/posts"
item = "article.post"
heading = "h2"
min_items = 1
```

If `min_items` is set and fewer than that many items are created, that feed fails.
This helps catch page-layout changes that would otherwise silently produce an empty feed.
`min_items = 0` is allowed and logs a warning.

JSON API feeds
--------------

`rsspls` can also generate RSS from JSON API endpoints using jq-like `jaq` filters.
Set `source = "json"` inside `[feed.config]` and use explicit iteration in `item` such as
`.items[]`.

For JSON feeds:

- `item` runs against the whole JSON document.
- `heading`, `link`, `summary`, `date.selector`, and `media` run against each item.
- `link` is required.
- `item` uses explicit iteration, so prefer `.items[]` over `.items`.
- Non-string values are serialized as JSON text.

JSON API examples
-----------------

### Generic JSON API

```
[[feed]]
title = "Example API"
filename = "example-api.xml"

[feed.config]
source = "json"
url = "https://example.com/api/posts"
item = ".posts[]"
heading = ".title"
link = ".url"
summary = [".summary", ".meta"]
```

### Lobsters Rust posts with score >= 15

```
[[feed]]
title = "Lobsters Rust (score >= 15)"
filename = "lobsters-rust.rss"

[feed.config]
source = "json"
url = "https://lobste.rs/t/rust.json"
item = ".[] | select(.score >= 15)"
heading = ".title"
link = ".url"
summary = [".description_plain", '"Score: \(.score) | Comments: \(.comment_count) | Tags: \(.tags | join(", ")) "', ".comments_url"]
date = ".created_at"
```

### USGS significant earthquakes of the week feed

```
[rsspls]
output = "/tmp"

[[feed]]
title = "Significant Earthquakes of the Week"
filename = "earthquakes.rss"

[feed.config]
source = "json"
url = "https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary/significant_week.geojson"
item = ".features[]"
heading = ".properties.title"
link = ".properties.url"
summary = [".properties.title"]
date = ".properties.time"
```

Build From Source
-----------------

**Minimum Supported Rust Version:** 1.70.0

`rsspls` is implemented in Rust. See the Rust website for [instructions on
installing the toolchain][rustup].

### From Git Checkout or Release Tarball

Build the binary with `cargo build --release --locked`. The binary will be in
`target/release/rsspls`.

### From crates.io

`cargo install rsspls`

Credits
-------

* [RSS feed icon](http://www.feedicons.com/) by The Mozilla Foundation

Licence
-------

This project is dual licenced under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/wezm/rsspls/blob/master/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/wezm/rsspls/blob/master/LICENSE-MIT))

at your option.

[rustup]: https://www.rust-lang.org/tools/install
[website]: https://rsspls.7bit.org/
