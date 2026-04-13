Make a VR Video App

Features:

[*] Play videos of various formats
  [*] 2D videos (flat and curved)
  [ ] 3D videos (side by side, top bottom)
  [*] 180 videos (side by side, top bottom)
    [*] panoramic
    [*] fish eye
  [*] 360 videos (side by side, top bottom)
    [*] panoramic
    [*] fish eye
    [ ] cubemap

[*] Has control panel with row of icons, a que, and the name of the video

  +--+--+--+--+--+--+--+
  |  |  |  |  |  |  |  |  <- icons
  +--+--+--+--+--+--+--+
  |   name of video    |
  +--------------------+
  |<---*--------->|time|  <- scrubber and time
  +--------------------+

  The icons include

  [*] previous video  - goes to previous video
  [*] play/pause      - starts and pauses current video
  [*] next video      - goes to next video
  [*] slow motion     - cycles 1x 2/3rd 1/2rd 1/3rd 1/4th
  [*] loop            - 1st click sets start, 2nd click sets end and start looping, 3rd click clears loop
  [*] settings        - brings up settings panel
  [*] list videos     - brings up list of videos (directory listing)
  [*] exit            - exits app

[*] Settings:

  [*] lets you pick the video format (see above)
  [*] lets you Left/Right Right/Left Top/Bottom Bottom/Top
  [*] lets you choose a zoom level

[*] Listing

  +------------------------+
  |[V][current path    ][X]|  V = volumes icon
  +------------------------+  X = close
  | file 1               |S|  S = scrollbar
  | file 2               | |
  | file 3               | |
  | file 4               | |
  +----------------------+-+

  [*] shows list of videos with a scroll bar
  [*] controller stick scrolls as well
  [*] lets you select a video or a folder
    [*] selecting a video plays the video
    [*] selecting a folder goes to that folder
    [*] shows ".." folder for going up to the parent folder

[*] App shows 2 long pointers, one for each hand, with a mark on where they are touching
  the panel. Pressing the 2nd (y, b) shows/hides the current panel. Then no panel is visible
  the pointer disappear. (a,x) selects (click an icon, drag the que)

[ ] We should use OpenXR's layers for the video
  [ ] XrCompositionLayerProjection
  [ ] XR_KHR_composition_layer_equirect2
  [ ] XR_KHR_composition_layer_cylinder
[ ] We should use OpenXR's layers for the panels
[*] long thin cylinder will work for each pointer

---

[*] fix exit
[*] left/right on VR stick should fast forward / rewind
[*] show logo in desktop window
[*] should align to player's initial position (but only around virtual axis for initial orientation)
[*] should remember settings per video (need to be saved somewhere)
[*] remember the last folder you were in.
[ ] should support (add to browser)

  Note: this feature will require a UI for URL-path, username, password
  so probably a [+] icon at the top where you can add other roots. This
  would really be an android/quest feature but could prototype on Windows.

  [ ] folders
  [ ] webdav
  [ ] http

[*] have button to select roots on listing panel
[*] if arg is folder start on listing at folder
[*] if no arg start on last folder - if error or last folder start on user's home (directories)
[*] make Y/B exit listing and settings.
[*] better icons
[*] handle unicode fonts
[*] make it handle 8x video
[*] make audio work when not 1x
[*] turn off pointers when no panel
[*] make it use previous video's settings if no settings.
[*] fix listing, que, mouse capture for slider
[*] no warnings
[*] debug log
[*] second pointer doesn't work
[*] fix more color issues.
[*] center stuff on top of listings
[*] add a border
[*] make stuff same size
[*] increase icon size
[*] show an error if can't read video?
[*] reposition scrubber and time
[ ] Make app icon larger
[*] Portrait videos are too big
[*] Videos should loop
[*] Cache network folder reads
[*] Make next/prev work for network
[ ] Add Refresh button in the listings folder (because it's cached)
[*] Icon in windows task bar
[*] webm
[*] installer
[*] Show the version somewhere

[ ] Make it run on Quest - add SMB support
  [ ] need settings to have way to add "roots" where each root is a SMB or webdav or HTTP


[*] add github actions to build
  [*] for Windows PC
  [ ] for Quest



