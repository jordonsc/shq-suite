"""Constants for CFA Fire Ban integration."""

DOMAIN = "cfa_fire_ban"

FEED_URL_TEMPLATE = (
    "https://www.cfa.vic.gov.au/cfa/rssfeed/{district}-firedistrict_rss.xml"
)

# Maps config slug to the label used in the RSS feed description.
DISTRICT_LABELS = {
    "central": "Central",
    "mallee": "Mallee",
    "wimmera": "Wimmera",
    "southwest": "South West",
    "northern_country": "Northern Country",
    "northeast": "North East",
    "gippslandeast": "East Gippsland",
    "westandsouthgippsland": "West and South Gippsland",
    "northcentral": "North Central",
}

VALID_DISTRICTS = list(DISTRICT_LABELS.keys())
