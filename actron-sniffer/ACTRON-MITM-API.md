This document is a design plan for Claude. It may be deleted after implementation.

The plan is to take the work of the `actron-sniffer` application and turn it into a dual-purpose ESP32 application.
Additional, we'll deliver a Home Assistant custom component to interact with our new API.

We will be enhancing the current ESP32 application, not creating a new application.

# Application Duality
The duality of the ESP32 device will be:

a. Reverse engineering toolkit
b. MITM API

Moving forward, the nomenclature of these two APIs will be:
* RE API (HTTP REST reverse engineering toolkit)
* Controller API (Websockets client-facing API for normal usage)

## 1. RE API
The current HTTP REST endpoints will continue to be used as a reverse engineering toolkit. These may remain untouched
beyond any cleaning up required.

## 2. Controller API
The device will act as an Actron controller using a MITM model to allow us to create a local-network integration that
can be adopted into the limited flexibility of the Actron architecture. 

The API will be client-facing to home automation systems such as Home Assistant. It will NOT be HTTP REST, but instead
use Websockets for real-time push updates and client commands. It is important that the Websockets API does not 
interfere in the RS485 communication. The HTTP API already considers this, and is specifically designed not to disturb
real-time RS485 traffic.

# Controller API
The new Websockets API is what the Home Assistant component will interact with. It will be able to:

* Display and control the master mode
* Display and control the master temperature
* List all zones with:
  * Their names
  * Their current temperatures and their target temperatures for the CURRENT operating mode (cool, heat, auto)
  * If each zone is enabled or disabled
* Enable and disable zones
* Set the target temp for each zone for the CURRENT mode (heat, cool, auto)

Things to note:
* The Actron has per-mode setpoints. On the API, we're going to act as if there is a single setting, which switches to
  new values when we change mode. This is consistent with how most clients, including the NEO display itself, operate.
* Not all modes have temperature values. Off & Fan modes don't have a temperature map, so the values should be 
  flagged as `optional`.

There is also a period between command execution and the 'local board' publishing the new value. During this period the
API should behave as follows:
* It returns the currently published mode/value for the attribute
* The Websockets server will remember the command it just sent, and include in the response a "transition" value.

Example:
* I set mode from HEAT to COOL
* ESP32 transmits the new mode change
* Local board continues to publish "HEAT" as the mode
* WS API shows "HEAT" as the current mode
* WS API includes a `mode-transitioning` value of "COOL" for a max grace period of `GRACE_PERIOD`
* When the local board starts showing the mode as COOL, the WS API now publishes mode as COOL and nullifies the
  `mode-transitioning` value
* If `GRACE_PERIOD` expires without a mode change from the local board, we nullify the `mode-transitioning` value
  and continue publishing the value we see the local board broadcasting

`GRACE_PERIOD` should be a code-level constant set to 60 seconds.

The Websockets API should inject changes to NEO's response payload using the INJECT mechanism, not the RESPONSE 
mechanism. This is important as it should not overwrite live sensor data being sent from the NEO to the local board.

# Security
This device will operate in a trusted LAN environment. It will not use TLS. HTTP and WS traffic will be open without
any authentication mechanisms.

# Home Assistant component
The final piece to this plan is a NEW Home Assistant custom component. This repo has an existing HA component, it may 
be used as a reference for designing components with the HVAC device type, but otherwise should not be used as an
example.

The new HA component will be named "actron-mitm-controller". It will integrate with the Websockets API on a configurable
IP address. It will provide an HVAC device with zones, similar to the existing component. 

**Important!** Unlike the existing HA component, we will NOT be adding optimistic updates or retry mechanisms. The new
API is expected to be completely solid, and API-level errors should be reflected on the client.

The WS will publish a `*-transitioning` value for changes. The HA component will use these values in place of optimistic
updates. Example:

    ha_mode = ws_mode_transitioning if ws_mode_transitioning is not None else ws_mode

Out of scope for this project:
* Integrating quiet mode, turbo mode, continuous fan and away mode.
