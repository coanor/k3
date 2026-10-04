# K3 user guide

For installation shortcuts, new-version detection, and update/uninstall commands, see the [installation guide](install-packages.md#update-or-uninstall-an-online-installation).

**English** | [简体中文](user-manual.zh-Hans.md) | [繁體中文](user-manual.zh-Hant.md)

Prepare a song, sing along, and save your recording. This guide uses the desktop GUI;
button names below match the English interface. Change the interface language in **Settings**.

## 1. Install and open K3

The GUI is available on **Windows x64** and **Linux x64/ARM64**.
macOS and Windows ARM64 currently provide CLI/TUI only; see the
[project README](https://github.com/coanor/k3) for those platforms.

Install [v0.1.3](https://github.com/coanor/k3/releases/tag/v0.1.3):

Windows — open PowerShell and paste:

```powershell
irm -ErrorAction Stop https://github.com/coanor/k3/releases/download/v0.1.3/get.ps1 | iex
```

Linux — open a terminal and paste:

```bash
curl -fsSL https://github.com/coanor/k3/releases/download/v0.1.3/install.sh | bash -s -- --version v0.1.3
```

Choose a disk with **at least 8 GiB free**, enter a new or empty installation folder,
and confirm with `y`. Installation needs internet access. It downloads the programs,
Python and default separation models; you do not need to install them yourself.
A full installation usually uses 2–4 GiB afterward. Installer messages are in English.

Windows requires Windows 10/11. Linux requires glibc 2.39 or later and a graphical desktop.
On Ubuntu 24.04, install the required system libraries first:

```bash
sudo apt update && sudo apt install libasound2t64 libfontconfig1 libxkbcommon-x11-0 libegl1 libgl1-mesa-dri
```

When installation finishes, open **`k3-gui.exe`** on Windows or **`k3-gui`** on Linux
from your chosen installation folder. Keep the whole folder together.

## 2. Choose where to save your songs

On first launch, choose a **projects folder**. K3 stores prepared songs, lyrics and recordings
there. Choose a location with room for audio files, outside the K3 installation folder.
An empty folder is fine; select your existing projects folder if you already use K3.

The **gear button** opens Settings. Change the projects folder, volume, default separation
profile and interface language there. Changes save automatically; no configuration-file
editing is needed. K3 reopens your last project paused when you launch it again.

## 3. Add a song

1. Click **Separate song**, then **Choose files**. Select one or more audio files.
2. Keep **Quality** and **Runtime default** to start. Try **Balanced** or **Fast** if processing is too slow.
3. Click **Create and separate** for one new song, or **Queue selected** for several.
4. Wait for the prepared song to appear in the left-hand project list, then select it.

You can add more songs while K3 works. The progress card shows the current song and queue;
its percentage is for the current processing stage. Press **Esc** to return to playback:
preparation continues in the background. Click the progress card to reopen it.

You can play or record another project during preparation. A project whose stems are being
replaced is temporarily unavailable. **Replace stems** or **Queue & replace** recreates tracks
from that project's **saved original audio**, keeping lyrics and recordings. A newly selected
file with the same project name does not replace the saved original.

The default models work offline after installation. Optional models such as **Kim Vocal 2**
or **Compatible** may need additional downloads on first use. Separation can leave some
voice or instrument leakage; try another profile and replace stems if needed.

## 4. Listen and add lyrics

Click Play and select a track:

| Track | What you hear |
| --- | --- |
| **Original** | The original song |
| **Backing** | The accompaniment, with backing vocals preserved by default |
| **Vocals** | The separated lead vocal |
| **Take** | Your selected saved recording mixed with accompaniment |

Use the progress bar to seek, the volume slider to adjust sound, and the key controls to
raise or lower the song's pitch.

For synchronized lyrics, click **Find lyrics**, search by song title or artist, preview a
result, then click **Use this version**. Search needs internet access. Existing lyrics stay
unchanged until you save a result. You can cancel without replacing them.

## 5. Record yourself

1. Connect headphones and a microphone. Select your input/output devices in your operating system;
   K3 uses the default devices.
2. Open a prepared song with a Backing track. Choose an **Effect**; start with **Clean**.
3. Click the round **Record** button. K3 starts the backing track from the beginning and records your microphone.
4. The headphones button controls live microphone monitoring. Turn it off if you hear an echo or delay.
5. Click **Stop** and wait for saving/mixing to finish. The recording saves automatically.
6. Select it in the **Take** dropdown and choose the **Take** track to listen.

Each recording is saved as a separate take. Stopping early still keeps the accompaniment
through the end of the song in the mix. Closing K3 while recording stops and saves it before exiting.

Change **Effect** to rebuild the selected recording's mix; the original microphone recording
is preserved. Wait for processing to finish before switching takes or recording again.

## 6. Share or delete a recording

To share a recording, open your projects folder in the file manager, then open the song's
`takes` folder. Find WAV files with **`mix`** in the name (`take-…-mix.wav` or `mix-….wav`),
preview the recording you want, and copy it elsewhere to send it.
**`take-…-dry.wav`** contains only your original microphone recording. Keep the project files
in place so K3 can continue using them.

To delete a take, select it in **Take**, open **⋯**, and choose **Delete selected take…**.
The confirmation defaults to **Keep take**; Enter or Esc keeps it. Click **Delete take** to
remove that recording. Other takes, lyrics and song tracks remain.

Back up the **whole projects folder** to preserve your songs and recordings. When updating
K3, install into a new empty folder and choose your existing projects folder on launch.

## 7. Optional: songs from NetEase Cloud Music

In **Separate song**, enable the experimental NetEase source. Use **QR login** with the
NetEase phone app, or **Import Chrome login**. Search or open **Liked songs**, select songs,
then click **Queue selected songs**. Only download songs your account is allowed to access.

A yellow check means queued; green means the audio has downloaded. **DOWNLOADED** does not
mean separation has finished. Prepared songs appear in the project list. You can continue
adding songs while downloads and separation run. Log in again if the session expires.
This experimental source may stop working; local audio files remain an alternative.

## 8. If something goes wrong

| Problem | What to try |
| --- | --- |
| Installation reports low disk space | Choose another disk or free space; use a new or empty installation folder. |
| GUI will not open | Check the platform requirements and, on Linux, the system libraries above. Keep all installation files together. |
| No sound or microphone recording | Check the operating system's default devices and volume. Recording needs a prepared Backing track. |
| Echo while recording | Use headphones or turn off live monitoring. |
| Lyrics are missing | Use Find lyrics with a more specific song title and artist. |
| Separation fails | Check free space and internet access if a model download is needed; try Quality with Runtime default again. |

If the problem continues, open the **information button → About**, find the diagnostics log
path, and include that log and any copied error text when reporting the problem. A Windows
startup error dialog also gives the log path if the GUI cannot open.

Useful shortcuts when the main view has focus: **Space** play/pause, **1–4** switch tracks,
**R** start/stop recording, **M** toggle monitoring, **L** find lyrics, **Esc** close a panel.
