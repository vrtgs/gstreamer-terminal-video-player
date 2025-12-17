# video-less

This project is enables playing videos in the terminal!

like `less` but for videos

## Building

Follow the instructions on building [gstreamer]([https://github.com/zmwangx/rust-ffmpeg/wiki/Notes-on-building](https://gstreamer.freedesktop.org/documentation/rust/git/docs/gstreamer/index.html#installation)), as this crate depends on it. Then just do:

```rs
cargo run --release -- demo.mp4
```

To download the demo video:

```
yt-dlp -f mp4 https://www.youtube.com/watch?v=WO2b03Zdu4Q -o demo.mp4
```


you can also skip forwards and backwards the video or even pause
