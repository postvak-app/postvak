#!/usr/bin/env bash
# Add Postvak's Flatpak repository to the docs site, so `flatpak update` brings new releases.
#
# Usage: packaging/linux/flatpak-repo.sh SITE
#   SITE is the built site (docs/book); this writes SITE/flatpak/repo and SITE/flatpak/postvak.flatpakrepo. Every
#   Pages deploy replaces the whole site, so the repository is mirrored from the live site first, then the latest
#   release's bundles are imported into it. Needs flatpak, ostree, curl and gh (with GH_TOKEN and GITHUB_REPOSITORY);
#   $SITE_URL is the site's address. Before the first release with bundles, it writes nothing. A failure to read the
#   live repository or the release stops the script, so a deploy never drops the repository.
set -euo pipefail

site=${1:?usage: flatpak-repo.sh SITE}
app_id=io.github.postvak_app.postvak
base_url=${SITE_URL:?SITE_URL must be set}
base_url=${base_url%/}
repo=$site/flatpak/repo
bundles=$(mktemp -d)
trap 'rm -rf "$bundles"' EXIT

mkdir -p "$repo"
ostree --repo="$repo" init --mode=archive-z2
status=$(curl -sSL -o /dev/null -w '%{http_code}' "$base_url/flatpak/repo/summary")
case $status in
    200)
        ostree --repo="$repo" remote add --no-gpg-verify live "$base_url/flatpak/repo"
        ostree --repo="$repo" pull --mirror live
        ostree --repo="$repo" remote delete live
        ;;
    404) ;;
    *)
        echo "reading the live Flatpak repository failed with HTTP $status" >&2
        exit 1
        ;;
esac

tag=$(gh release view --repo "$GITHUB_REPOSITORY" --json tagName --jq .tagName)
# A release that flatpak.yml has not attached bundles to yet has none to download.
assets=$(gh release view "$tag" --repo "$GITHUB_REPOSITORY" --json assets --jq '.assets[].name')
if grep -q '^postvak-.*\.flatpak$' <<<"$assets"; then
    gh release download "$tag" --repo "$GITHUB_REPOSITORY" --pattern 'postvak-*.flatpak' --dir "$bundles"
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
# Each imported bundle is a root commit and the mirror copies only each ref's tip, so a ref holds just the latest
# release; pruning drops the objects of the one it replaced.
flatpak build-update-repo --title=Postvak --comment='Postvak releases' --homepage="$base_url/" \
    --default-branch=stable --prune "$repo"
cat >"$site/flatpak/postvak.flatpakrepo" <<EOF
[Flatpak Repo]
Title=Postvak
Comment=Postvak releases
Url=$base_url/flatpak/repo/
Homepage=$base_url/
EOF
echo "$app_id: $(ostree --repo="$repo" refs | tr '\n' ' ')"
