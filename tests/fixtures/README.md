# Generated audio fixture

`extended-mdat.m4a` contains a 0.2-second, 440 Hz sine wave generated for vtamp's tests. It contains no third-party music or artwork and is distributed under the project's MIT license.

It was generated with FFmpeg's `sine` source and AAC encoder. The adjacent eight-byte `free` atom and normal `mdat` header were replaced with a sixteen-byte, extended-size `mdat` header. File length and audio offsets are unchanged. This reproduces the MP4 layout in the local development samples without redistributing those recordings.

The regression test verifies both metadata extraction and actual AAC sample decoding. FFmpeg is not needed to run the tests or vtamp.

`stereo.wav`, `stereo-alac.m4a`, and `stereo-aac.m4a` contain the same original
0.3-second stereo signal: 440 Hz at amplitude 0.2 on the left and 880 Hz at
amplitude 0.1 on the right, sampled at 48 kHz. They are also MIT licensed and
contain no third-party recordings. WAV and ALAC verify that the shared decoder
preserves the lossless samples; AAC exercises native stereo decoding and seeking.

They were generated using FFmpeg with this input:

```sh
ffmpeg -f lavfi -i 'aevalsrc=0.2*sin(2*PI*440*t)|0.1*sin(2*PI*880*t):s=48000:d=0.3' -c:a pcm_s16le stereo.wav
ffmpeg -f lavfi -i 'aevalsrc=0.2*sin(2*PI*440*t)|0.1*sin(2*PI*880*t):s=48000:d=0.3' -c:a alac stereo-alac.m4a
ffmpeg -f lavfi -i 'aevalsrc=0.2*sin(2*PI*440*t)|0.1*sin(2*PI*880*t):s=48000:d=0.3' -c:a aac stereo-aac.m4a
```

## Generated video fixtures

`video-h264.mkv`, `video-av1.mkv`, and `video-vp9.mkv` are silent 64×64
Matroska files of FFmpeg's `testsrc2` pattern: two seconds at 10 fps, twenty
frames, keyframes at frames 0 and 10, no audio track. They stand in for the
`video.mkv` sidecar of a YouTube import in the HTTP API test and in the
iPhone app's demuxer and decoder tests. They are MIT licensed and contain no
third-party footage.

```sh
ffmpeg -f lavfi -i testsrc2=size=64x64:rate=10 -t 2 -c:v libx264 -g 10 -bf 0 -pix_fmt yuv420p -an -f matroska video-h264.mkv
ffmpeg -f lavfi -i testsrc2=size=64x64:rate=10 -t 2 -c:v libsvtav1 -preset 12 -g 10 -pix_fmt yuv420p -an -f matroska video-av1.mkv
ffmpeg -f lavfi -i testsrc2=size=64x64:rate=10 -t 2 -c:v libvpx-vp9 -g 10 -b:v 100k -row-mt 1 -pix_fmt yuv420p -an -f matroska video-vp9.mkv
```
