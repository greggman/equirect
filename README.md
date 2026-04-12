# Equirect

A Rust VR Video Player

[Download here](https://github.com/greggman/equirect/releases/latest)

Note: At the moment it will start in your home folder and, while you can
switch folders, you can not switch drives (on the todo list). You can start
it from the command line with a video or a folder though

---

Hopefully this is portable or a step at being portable. It uses WebGPU for the
graphics so could potential run on Metal. It uses OpenXR for VR parts.

Anyway, this is alpha software. I had Claude write it because
[my first VR video player](https://github.com/greggman/vrvp)
aged out. A few months ago it stopped working because Meta
updated some APIs.

I first tried [writing a WebXR player](https://github.com/greggman/webxr-video)
but that didn't end up working for me. I wouldn't have tried to do this except
for finally getting into this whole AI code generation thing. If that's not your
jam I totally get it.

I was pretty impressed though. I didn't write any of the code in this repo. It's
100% Claude. I did supervise for 20hrs or so. I worked through the issues with
Claude. I didn't review 100% of the code but I did catch Claude doing some
questionable things and got them re-worked. To give an example, at one point was
trying to get Claude to fix a UI issue. Claude came up with a solution and then
applied it at 7 different places in the code. It didn't make sense to fix it in
7 places. It should have been fixed in 1 place and have all 7 of those existing
places use the one fix. There were several issues like that, where Claude was
doing something a reviewer wouldn't let through.

Another is I wanted particular solutions. Like Claude initially made a sphere
mesh for projection and I was like, why? We do you need a sphere when you can
just do the projection in the shader. I pointed Claude at the WebXR version as
an example and it got a similar solution working.

On the other hand, Claude made this from scratch. Maybe the hardest part was
right at the beginning, Claude trying to get wgpu connected to OpenXR and having
to dig through lower level stuff. I have no rust, no OpenXR, and no wgpu
experience and I have no idea how long it would have taken to figure out how to
get those things connected.

What I've found at this point in time is writing plans and also asking Claude to
write plans, and then taking things one step at a time often seems to get good
results.

Of course this is a pretty limited scope app.

Note: This is alpha software. I plan to have Claude clean up the UI a little.
Put in graph icons (though still programmer art by me). Handle unicode
filenames in the display. Put a logo or something on the window that appears
on the desktop. I might try to separate out the video to GPU part into a
crate if that's useful. Maybe it could be the start of a wgpu 
[`importExternalTexture`](https://gpuweb.github.io/gpuweb/#dom-gpudevice-importexternaltexture) implementation crate.

## License: MIT
