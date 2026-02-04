Superhero HQ Tools
==================
The SHQ toolsuite is a set of applications for home automation which include:

 * Display helpers for wall displays (kiosks) and dashboard controls
 * Audio alarms & TTS services
 * Automated door control systems
 * Home Assistant components to control the above

Applications
------------
Directory structure for applications:

 * `nyx`: Tool to control the brightness and power state of wall displays (Rust)
 * `overwatch`: TTS server with an alarm loop - verbalises text prompts and raises alarms (Rust)
 * `dosa`: Door Opening Sensor Automation - automated door control via grblHAL CNC controller (Rust)
 * `home-assistant`: The Home Assistant custom components that integrate with the above tools (Python)
 * `deploy`: A tool to deploy each application to respective devices (symlink to `setup` in root) (Python)
 * `shelly`: CLI tool to discover, audit, and configure Shelly smart home devices on the local network (Python)

Deployment Configuration
------------------------
The `deploy/config` file includes some YAML files you need to complete in order to deploy to your devices.

The config includes a list of hostnames to devices & your Home Assistant server, along with user & private key
information, etc.


Kiosk Setup
-----------
The wall display is intended to be a _Raspberry Pi 5_ running Raspberry Pi OS, attached to a _Raspberry Pi Display 2_ 
LCD screen via DSI.

### Initial Setup
Set up the Pi using a consistent username. Ensure that you install with GUI support. Once setup, run the Raspberry Pi
config tool to configure the OS for kiosk use:

`raspi-config`

	-> System -> Hostname -> Set to "kioskXX" (no hyphens)
	-> System -> Auto-login -> Enable (inc. desktop)
	-> System -> Splash Screen -> Disable
	-> Interface -> SSH -> Enable

On your network controller, give the Pi DNS to use, prefixed with a kiosk name.

    # Example DNS:
    kiosk01.myhouse.dev

From there, you can run the deploy tool:

    ./deploy kiosk -h kiosk01.myhouse.dev

That will configure the Pi to run a dashboard with the display service installed. Home Assistant can then control
the brightness and on/off state of kiosk's screen with any automations you so choose to create.

> Pro-tips: Start Chromium manually once, set it to dark-mode and do NOT enter a keyring password (kiosk will use basic
> auth). Log into HA manually so that the kiosk will work seemlessly afterwards.

Overwatch Server Setup
----------------------
The Overwatch server is intended to be a Raspberry Pi 5 64-bit in console mode alone. Audio should be connected via USB
cable for clean digital audio.

Install the OS for console mode only, and perform similar setup to kiosks:

`raspi-config`

	-> System -> Hostname -> Set to "overwatch"
	-> System -> Auto-login -> Enable
	-> System -> Splash Screen -> Disable
	-> Interface -> SSH -> Enable

In the `overwatch/` directory, you can build the Rust application for either a local environment or a Raspberry Pi
release:

	cd overwatch
	
	# Setup pre-requisites on WSL2
	./setup-wsl2.sh
	source ~/.cargo/env 

	# Build & run locally
	cargo run

	# Build for R-Pi 64-bit
	./build-rpi.sh

	# Use the deployer tool to ship to the Pi:
	cd ..
	./setup overwatch

Shelly Device Management
------------------------
The Shelly tool discovers and configures Shelly devices on the local network via mDNS. It supports
both Gen1 and Gen2 APIs.

    cd shelly/src

    # Scan & display all devices
    python shelly.py

    # Initialise devices (disable cloud, BT, WiFi AP; set transition times)
    python shelly.py --init

    # Target a specific device
    python shelly.py -d <device_id> --init

    # Calibrate dimmers / trigger firmware updates
    python shelly.py --calibrate
    python shelly.py --update

See [docs/Shelly.md](docs/Shelly.md) for full documentation.

### Overwatch Sounds
Overwatch sounds are categorised in one of two taxonomies:

 * Tones (chimes at the start of verbalisations)
 * Alarms (designed to be played in a loop)

These currently site in `overwatch/sounds/` and are synced from there to the Overwatch server.
