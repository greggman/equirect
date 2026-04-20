# Equirect

<img alt="Equirect logo" src="resources/images/equirect.svg" width="400">

A Privacy First Rust VR Video Player

[Download here](https://github.com/greggman/equirect/releases/latest) (exe or installer, both should work)

[Instructions Below](#instructions)

## Privacy First

Unlike other VR video apps, this app does **ZERO DATA COLLECTION!**
Apps like DeoVR claim to monitor and share almost all viewing.
Apps like SkyboxVR, Pigasus, and most others claim to monitor less
but still not zero.

## Making Of Notes

Hopefully this is portable or a step at being portable. It uses WebGPU for the
graphics so could potentially run on Metal. It uses OpenXR for VR parts.

I had Claude write it because
[my first VR video player](https://github.com/greggman/vrvp)
aged out. A few months ago (January 2026) it stopped working because Meta
deprecated something. No idea what.

I first tried [writing a WebXR player](https://github.com/greggman/webxr-video)
but that didn't end up working for me. I wouldn't have tried to do this except
for finally getting into this whole AI code generation thing. If that's not your
jam I totally get it.

I was pretty impressed though. I didn't write any of the code in this repo. It's
almost 100% Claude. I did supervise for 20hrs or so. I worked through the issues with
Claude. I didn't review 100% of the code but I did catch Claude doing some
questionable things and got them re-worked. To give an example, at one point was
trying to get Claude to fix a UI issue. Claude came up with a solution and then
applied it at 7 different places in the code. It didn't make sense to fix it in
7 places. It should have been fixed in 1 place and have all 7 of those existing
places use the one fix. There were several issues like that, where Claude was
doing something a reviewer wouldn't let through.

Another is I wanted particular solutions. Like Claude initially made a sphere
mesh for projection and I was like, why? Why do you need a sphere when you can
just do the projection in the shader. I pointed Claude at the WebXR version as
an example and it got a similar solution working.

On the other hand, Claude made this from scratch. Maybe the hardest part was
right at the beginning, Claude trying to get wgpu connected to OpenXR and having
to dig through lower level stuff. I have no rust, no OpenXR, and no wgpu
experience and I have no idea how long it would have taken to figure out how to
get those things connected. Some other places that Claude figured out that I
suspect would have been hard, enabling hardware accelerated video decoding,
and dealing with YUV issues related to used-size being different than physical-size.

What I've found at this point in time is writing plans and also asking Claude to
write plans, and then taking things one step at a time often seems to get good
results.

Of course this is a pretty limited scope app.

I might try to separate out the video to GPU part into a
crate if that's useful. Maybe it could be the start of a wgpu 
[`importExternalTexture`](https://gpuweb.github.io/gpuweb/#dom-gpudevice-importexternaltexture) implementation crate.

## Instructions

Run it, it should open your home folder. Navigate to a video and it should play.
I've only tested on an Oculus Rift-S so I might need some PRs for other devices but.
The controls are supposed to be

* `B` / `Y` make the control panel appear / disappear
* `A` / `X` select a button
* `grip` reset-view

The control panel has the following icons

```
[prev][play/pause][next][speed][loop][settings][browse][exit]
```

* `prev` = go to previous video in same folder as current video
* `play\pause` = play and pause
* `next` = go to next video in the same folder as current video
* `speed` = 1x, 2/3x, 1/2x, 1/3x, 1/4x
* `loop` = 1 click sets start, 2nd click sets end, 3rd click turns off looping.
* `settings` = lets you pick projection mode
* `browse` = lets you pick videos by name
* `exit` = exit the app

If you pick a video and the projection is wrong, then go to setting
and pick the correct settings for your video. They'll be remembered.
When you chose a video that doesn't have settings it will default to
the settings of the last video you played.

The next time you run the app it will start in the browse UI
at the last folder you used.

#### Reset View

The app tries to always start the way you are facing, not the orientation
of your setup like many apps. Further, the 'grip' buttons reset the view
to your current view *IN 3D*. That means if you look up at the ceiling and
press the `grip` button, the ceiling will now be where you need to look.
This means you can orient the video anywhere that is comfortable. Just look
the direction you want to be the *neutral* position, then press `grip`.

### Command Line

You can run the app from the command line and pass in either
a path to file, which it will play. Or, you can pass a path to
a folder, in which case it will start with the browse UI to let you
pick a video.

You can also pass an network URL like `http://<local-ip-address>/path/somevideo.mp4`
You can run a server like [servez](https://github.com/greggman/servez) or
nginx etc and to should let you access your videos.

* `-v` or `--verbose`

  Verbose output

* `-t time` or `--start time`

  Starts the given video at a specific time

  ```
  -t 12:34     # start at 12 minutes 34 seconds
  -t 12:34:56  # start at 12 hours 34 minutes 56 seconds
  -t 123       # start at 123 seconds in same as (2:03)
  ```

## Development

To re-generate the icon texture atlas and the logo

```sh
npx svg-texture-atlas 128 resources/icons resources/icons
npx @greggman/svg-to-png resources/images/equirect.svg resources/equirect.png 640
npx @greggman/svg-to-png resources/images/equirect.svg resources/images/icon-32x32.png 32 32
npx @greggman/svg-to-png resources/images/equirect.svg resources/images/icon-128x128.png 128 128
```

Yea, I should add that to build but I don't expect to have to regenerate them often.

## License: MIT
