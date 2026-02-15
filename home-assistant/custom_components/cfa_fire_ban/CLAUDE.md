# CFA Fire Ban

Home Assistant integration that polls CFA (Country Fire Authority) RSS feeds for Total Fire Ban status and Fire Danger Ratings in Victoria, Australia.

## Entities

| Entity | Type | Description |
|--------|------|-------------|
| `binary_sensor.cfa_fire_ban_{district}_total_fire_ban` | Binary Sensor | On when a Total Fire Ban is active |
| `sensor.cfa_fire_ban_{district}_fire_danger_rating` | Sensor | Current rating: NO RATING / MODERATE / HIGH / EXTREME / CATASTROPHIC |

Both sensors expose `date` as an extra state attribute.

## Config

```yaml
cfa_fire_ban:
  district: central        # optional, default central
  name: "CFA Fire Ban"     # optional
```

Single-district only (flat schema, not multi-device dict).

## Data Source

- RSS feed: `https://www.cfa.vic.gov.au/cfa/rssfeed/{district}-firedistrict_rss.xml`
- Polled every hour via `DataUpdateCoordinator`
- First `<item>` = today's data

## Valid Districts

`central`, `mallee`, `wimmera`, `southwest`, `northern_country`, `northeast`, `gippslandeast`, `westandsouthgippsland`, `northcentral`

## Key Files

| File | Purpose |
|------|---------|
| `const.py` | Domain, district slug-to-label mapping, feed URL template |
| `coordinator.py` | `DataUpdateCoordinator` — fetches RSS, parses XML for TFB + rating |
| `binary_sensor.py` | Total Fire Ban on/off sensor |
| `sensor.py` | Fire Danger Rating text sensor |

## Parsing

- **TFB**: Checks description for `"is not currently a day of"` (no ban) vs presence of `"Total Fire Ban"` (active)
- **Rating**: Regex matches `"{District Label}: {RATING}"` pattern in description HTML
