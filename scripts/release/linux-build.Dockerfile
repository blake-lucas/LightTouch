# The Debian 11 (glibc 2.31) image the release's Linux engine and its dependency prefixes build in: what the local
# image lighttouch-bullseye-collector08 holds (the packages and Python tools of the former
# Linux/packaging/build-bullseye-inner.sh), as a recipe, so CI can make it (.github/workflows/release-build.yml).
#   docker build -t lighttouch-linux-build -f scripts/release/linux-build.Dockerfile scripts/release
# Then LT_LINUX_IMAGE=lighttouch-linux-build for scripts/build-release.sh's linux step.
FROM debian:bullseye
# Debian 11 is past its end of life: its packages, security updates included, are on archive.debian.org.
RUN printf 'deb [check-valid-until=no] http://archive.debian.org/debian bullseye main\ndeb [check-valid-until=no] http://archive.debian.org/debian-security bullseye-security main\n' \
        > /etc/apt/sources.list && rm -f /etc/apt/sources.list.d/* \
    && apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        build-essential pkg-config ninja-build python3 python3-dev python3-venv python3-pip python3-tk git \
        ca-certificates curl patch bison flex gawk gettext libsdl2-dev libglib2.0-dev libpixman-1-dev zlib1g-dev \
        libegl1-mesa-dev libgl1-mesa-dev libfdt-dev autoconf automake libtool libusb-1.0-0-dev libssl-dev \
        libcurl4-openssl-dev patchelf squashfs-tools desktop-file-utils \
    && rm -rf /var/lib/apt/lists/* \
    && python3 -m pip install --no-cache-dir cmake==3.27.9 packaging==24.0 tomli==2.0.1 meson==1.10.0 pycotap==1.3.1
