// HTTP debug / RE / operator API (SPEC §8). Read endpoints are GET; mutating endpoints are
// POST (so they can't be triggered by browser prefetch / crawlers and params don't leak into
// GET-logged URLs). All open on the LAN — the security boundary is the restricted network
// (SPEC §7). /send is the RE workhorse: hand-craft any SDN message and watch the reply.

#pragma once

#include <cstdint>

namespace http_api {

void begin(uint16_t port);
void loop();

}  // namespace http_api
