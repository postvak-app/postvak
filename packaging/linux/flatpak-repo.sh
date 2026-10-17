#!/usr/bin/env bash
# Add Postvak's Flatpak repository to the docs site, so `flatpak update` brings new releases.
#
# Usage: packaging/linux/flatpak-repo.sh SITE
#   SITE is the built site (docs/book); this writes SITE/flatpak/repo and SITE/flatpak/postvak.flatpakrepo. Every
#   Pages deploy replaces the whole site, so the repository is mirrored from the live site first, then the latest
#   release's bundles are imported into it. Needs flatpak, ostree, curl and gh (with GH_TOKEN and GITHUB_REPOSITORY);
#   $PAGES_BASE_URL is the site's address. Before the first release with bundles, it writes nothing.
set -euo pipefail

site=${1:?usage: flatpak-repo.sh SITE}
app_id=io.github.postvak_app.postvak
base_url=${PAGES_BASE_URL:?PAGES_BASE_URL must be set}
base_url=${base_url%/}
repo=$site/flatpak/repo
bundles=$(mktemp -d)
trap 'rm -rf "$bundles"' EXIT

mkdir -p "$repo"
ostree --repo="$repo" init --mode=archive-z2
if curl -fsSL -o /dev/null "$base_url/flatpak/repo/summary"; then
    ostree --repo="$repo" remote add --no-gpg-verify live "$base_url/flatpak/repo"
    ostree --repo="$repo" pull --mirror live
    ostree --repo="$repo" remote delete live
fi

tag=$(gh release view --repo "$GITHUB_REPOSITORY" --json tagName --jq .tagName 2>/dev/null || true)
if [ -n "$tag" ]; then
    gh release download "$tag" --repo "$GITHUB_REPOSITORY" --pattern 'postvak-*.flatpak' --dir "$bundles" || true
fi
for bundle in "$bundles"/postvak-*.flatpak; do
    [ -e "$bundle" ] || continue
    echo "importing $(basename "$bundle") from $tag"
    flatpak build-import-bundle "$repo" "$bundle"
done

if [ -z "$(ostree --repo="$repo" refs)" ]; then
    echo "no Flatpak release yet; the site gets no repository"
    rm -rf "$site/flatpak"
    exit 0
fi
# A few commits per ref are enough to roll back with `flatpak update --commit`, and keep the site small.
flatpak build-update-repo --title=Postvak --comment='Postvak releases' --homepage=https://postvak.pataar.nl/ \
    --default-branch=stable --prune --prune-depth=3 "$repo"
cat >"$site/flatpak/postvak.flatpakrepo" <<EOF
[Flatpak Repo]
Title=Postvak
Comment=Postvak releases
Url=$base_url/flatpak/repo/
Homepage=https://postvak.pataar.nl/
EOF
# GitHub Pages leaves out files and folders whose names start with an underscore unless Jekyll is off.
touch "$site/.nojekyll"
echo "$app_id: $(ostree --repo="$repo" refs | tr '\n' ' ')"
