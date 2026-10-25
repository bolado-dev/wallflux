#!/usr/bin/env python3
"""Genera catalog.json a partir de Wikimedia Commons.

El catálogo curado de wallflux no se escribe a mano: se genera desde la API
abierta de Commons, que no pide clave, y se filtra a vídeo con licencia
permisiva, resolución decente y un tamaño que no tarde una eternidad en
descargarse. Cada URL se comprueba con una petición HEAD antes de entrar, así
que el fichero que se publica no contiene enlaces rotos.

    ./catalog/build.py > catalog/catalog.json

Para añadir temas, edita QUERIES. Para añadir un fondo concreto que no salga
en ninguna búsqueda, añádelo a mano al final del JSON: el formato es estable.
"""

import html
import json
import re
import sys
import urllib.parse
import urllib.request

API = "https://commons.wikimedia.org/w/api.php"
UA = "wallflux-catalog/0.1 (https://github.com/bolado-dev/wallflux)"

# Temas que funcionan como fondo de escritorio: movimiento lento, en bucle
# natural, sin texto ni personas en primer plano.
QUERIES = [
    "timelapse aurora",
    "timelapse night sky stars",
    "timelapse clouds",
    "ocean waves slow motion",
    "lava flow",
    "underwater coral",
    "forest fog",
    "rain window",
    "snowfall",
    "timelapse city night",
    "abstract fluid",
    "fireplace fire",
    "waterfall",
    "clouds sky timelapse",
    "sunset timelapse",
    "milky way timelapse",
    "jellyfish",
    "river water flowing",
    "northern lights",
    "storm clouds",
    "fog mountains",
    "autumn leaves",
    "snow mountains",
    "sea waves beach",
]

# Solo licencias que permiten uso y redistribución sin pedir permiso. CC BY y
# BY-SA exigen atribución, y por eso el autor y el enlace a la ficha viajan en
# el manifiesto de cada fondo descargado.
ALLOWED_LICENSES = re.compile(
    r"^(CC0|CC BY(-SA)? [234]\.0|CC BY(-SA)? [23]\.0|Public domain|PD)", re.IGNORECASE
)

MIN_WIDTH = 1280
MAX_BYTES = 90 * 1024 * 1024  # 90 MB: un fondo no debería tardar en bajar
PER_QUERY = 12


def api_get(params):
    params = {**params, "format": "json"}
    url = f"{API}?{urllib.parse.urlencode(params)}"
    request = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response)


def strip_html(text):
    """Commons devuelve los metadatos con marcado; aquí solo queremos texto."""
    return html.unescape(re.sub(r"<[^>]+>", "", text or "")).strip()


def url_ok(url):
    request = urllib.request.Request(url, method="HEAD", headers={"User-Agent": UA})
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            return response.status == 200
    except Exception:
        return False


def search(query):
    data = api_get(
        {
            "action": "query",
            "generator": "search",
            "gsrsearch": f"filetype:video {query}",
            "gsrnamespace": 6,
            "gsrlimit": PER_QUERY,
            "prop": "imageinfo",
            "iiprop": "url|size|mime|extmetadata",
            "iiurlwidth": 640,
        }
    )
    return ((data.get("query") or {}).get("pages") or {}).values()


def to_item(page, tag):
    info = (page.get("imageinfo") or [{}])[0]
    meta = info.get("extmetadata") or {}

    if info.get("mime") != "video/webm":
        return None
    if (info.get("width") or 0) < MIN_WIDTH:
        return None
    if (info.get("size") or 0) > MAX_BYTES:
        return None

    license_name = strip_html((meta.get("LicenseShortName") or {}).get("value"))
    if not ALLOWED_LICENSES.match(license_name or ""):
        return None

    title = re.sub(r"^File:", "", page["title"])
    title = re.sub(r"\.(webm|ogv|mp4)$", "", title, flags=re.IGNORECASE)
    title = re.sub(r"\s+", " ", title.replace("_", " ")).strip()

    video_url = info.get("url")
    if not video_url or not url_ok(video_url):
        return None

    return {
        "id": f"commons-{page['pageid']}",
        "title": title[:80],
        "author": strip_html((meta.get("Artist") or {}).get("value")) or None,
        "license": license_name,
        "source_url": info.get("descriptionurl"),
        "video_url": video_url,
        "preview_url": info.get("thumburl"),
        "tags": [tag],
        "width": info.get("width"),
        "height": info.get("height"),
        "duration_secs": None,
        "source": "curated",
    }


def main():
    items, seen = [], set()
    for query in QUERIES:
        tag = query.split()[0]
        print(f"  {query}…", file=sys.stderr, end="", flush=True)
        found = 0
        try:
            pages = search(query)
        except Exception as err:
            print(f" fallo: {err}", file=sys.stderr)
            continue
        for page in pages:
            item = to_item(page, tag)
            if item and item["id"] not in seen:
                seen.add(item["id"])
                items.append(item)
                found += 1
        print(f" {found}", file=sys.stderr)

    items.sort(key=lambda item: item["title"].lower())
    json.dump({"version": 1, "items": items}, sys.stdout, indent=2, ensure_ascii=False)
    print(file=sys.stdout)
    print(f"\n{len(items)} fondos verificados", file=sys.stderr)


if __name__ == "__main__":
    main()
