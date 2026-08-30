#!/usr/bin/env bash
# Post-processes a built mdBook for pdfboss.dev/docs: a canonical link on
# every page, noindex on the print view, and a sitemap of the chapters.
set -euo pipefail

book="${1:-docs/book}"
base="https://pdfboss.dev/docs"
sitemap="$book/sitemap.xml"

index_copies=""
for file in "$book"/*.html; do
  name="$(basename "$file")"
  [ "$name" = "index.html" ] && continue
  cmp -s "$file" "$book/index.html" && index_copies="$index_copies $name"
done

{
  echo '<?xml version="1.0" encoding="UTF-8"?>'
  echo '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">'
} > "$sitemap"

find "$book" -name '*.html' | sort | while read -r file; do
  rel="${file#"$book"/}"
  case " $rel " in
    " 404.html "|" toc.html ") continue ;;
    " print.html ")
      sed -i.bak 's|<head>|<head><meta name="robots" content="noindex">|' "$file"
      rm "$file.bak"
      continue ;;
  esac
  url="$base/$rel"
  listed=1
  if [ "$rel" = "index.html" ]; then
    url="$base/"
  fi
  case " $index_copies " in
    *" $rel "*) url="$base/"; listed=0 ;;
  esac
  sed -i.bak "s|<head>|<head><link rel=\"canonical\" href=\"$url\">|" "$file"
  rm "$file.bak"
  [ "$listed" = 1 ] && echo "  <url><loc>$url</loc></url>" >> "$sitemap"
done

echo '</urlset>' >> "$sitemap"
echo "canonicals added, sitemap at $sitemap ($(grep -c '<loc>' "$sitemap") urls)"
