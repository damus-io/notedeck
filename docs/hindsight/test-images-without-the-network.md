# A test helper that puts a picture in the Images cache

A test drawing a profile picture waits on the network or sees the loading circle;
a real one means hand-seeding the disk cache and polling until it loads.

- headway:notedeck/fall-slide-distance — ~10 min: the rich-row snapshot reads `MediaCache::write` + `static_imgs.cache_dir` + `MediaCache::key` out of imgcache.rs and static_imgs.rs to seed `RICH_PFP_URL` (an unanswered 127.0.0.1 URL), then polls `get_or_request` for `Loaded`
- headway:notedeck/ball-lawn-zoo — ~5 min: notedeck_ui/tests/markdown_images.rs copies command_bar.rs's `MediaCache::write` into `static_imgs.cache_dir` under an https://127.0.0.1:9 URL, and ticks the bare host each frame so the media job delivers the texture
