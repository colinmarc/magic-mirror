#!/bin/sh -e

die() {
    RED="\033[31m"
    RESET="\033[0m"
    echo -e "${RED}$1${RESET}"
    exit 1
}

case $1 in
"client" | "server")
    component=$1
    ;;
    *)
    die "invalid component: $1"
    exit 1
    ;;
esac

if [ -n "$(git status --untracked-files=no --porcelain)" ]; then
    die "working directory not clean; exiting"
    exit 1
fi

branch="auto-bump-${component}"
git fetch -q origin "${branch}"
tag="$(git show -s --format=%s origin/${branch} | awk '{print $NF}')"
if [ -n "$(git tag | grep ${tag})" ]; then
    die "tag exists"
fi

echo "bumping mm${component} to ${tag}..."
git cherry-pick -S "origin/${branch}"
git tag ${tag}
git show



