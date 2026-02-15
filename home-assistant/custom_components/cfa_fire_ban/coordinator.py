"""DataUpdateCoordinator for CFA Fire Ban integration."""
import logging
import re
import xml.etree.ElementTree as ET
from datetime import timedelta

from homeassistant.core import HomeAssistant
from homeassistant.helpers.aiohttp_client import async_get_clientsession
from homeassistant.helpers.update_coordinator import DataUpdateCoordinator, UpdateFailed

from .const import DISTRICT_LABELS, FEED_URL_TEMPLATE

_LOGGER = logging.getLogger(__name__)


class CfaFireBanCoordinator(DataUpdateCoordinator):
    """Coordinator to fetch and parse CFA fire ban RSS feed."""

    def __init__(self, hass: HomeAssistant, district: str) -> None:
        """Initialise the coordinator."""
        self.district = district
        self.district_label = DISTRICT_LABELS[district]
        self._url = FEED_URL_TEMPLATE.format(district=district)

        super().__init__(
            hass,
            _LOGGER,
            name=f"CFA Fire Ban ({self.district_label})",
            update_interval=timedelta(hours=1),
        )

    async def _async_update_data(self) -> dict:
        """Fetch RSS feed and parse fire ban data."""
        session = async_get_clientsession(self.hass)

        try:
            resp = await session.get(self._url, timeout=30)
            resp.raise_for_status()
            text = await resp.text()
        except Exception as err:
            raise UpdateFailed(f"Failed to fetch CFA RSS feed: {err}") from err

        try:
            root = ET.fromstring(text)
        except ET.ParseError as err:
            raise UpdateFailed(f"Failed to parse CFA RSS XML: {err}") from err

        item = root.find(".//item")
        if item is None:
            raise UpdateFailed("No items found in CFA RSS feed")

        title = item.findtext("title", "")
        description = item.findtext("description", "")

        tfb_active = self._parse_tfb(description)
        fire_danger_rating = self._parse_rating(description)

        _LOGGER.debug(
            "CFA %s: date=%s, tfb=%s, rating=%s",
            self.district_label, title, tfb_active, fire_danger_rating,
        )

        return {
            "date": title,
            "tfb_active": tfb_active,
            "fire_danger_rating": fire_danger_rating,
        }

    def _parse_tfb(self, description: str) -> bool:
        """Parse Total Fire Ban status from description HTML."""
        if "is not currently a day of" in description:
            return False
        if "Total Fire Ban" in description:
            return True
        return False

    def _parse_rating(self, description: str) -> str:
        """Parse fire danger rating from description HTML."""
        pattern = re.escape(self.district_label) + r":\s*([A-Z ]+)"
        match = re.search(pattern, description)
        if match:
            return match.group(1).strip()
        return "Unknown"
