# Build image for SpritzWine's CI (runner label `spritzwine-local`).
#
# Why an image at all, when the old .woodpecker.yml just `dnf install`ed at the top of every
# job: because Wine's configure SILENTLY disables any backend whose dev headers are missing.
# It does not warn, and it does not fail -- it just builds a Wine without that driver. The
# old package list contained no libX11-devel at all, so the release tarball that lane would
# have published had NO X11 driver, and nothing anywhere would have said so.
#
# Pinning the dependency set here (and asserting the result in ci/smoke.sh, which FAILS the
# build if a required backend went missing) turns that class of bug from invisible into loud.
# It also stops every job from re-resolving ~90 packages against a moving mirror.
#
# Build it with ./ci/rebuild.sh -- it re-stamps FIRST (the stamp is baked into the image as
# the last layer, so it must describe the Dockerfile being built), builds, verifies the bake,
# and prunes the dangling layers a rebuild leaves behind.
FROM fedora:44

# Two overlapping dependency sets live in this image.
#
# (a) Wine. new-WoW64 needs NO 32-bit ELF packages: the PE side is real PE, built by
#     clang+lld, over a single 64-bit Unix side. That is why nothing below is an i686
#     package, and why this image is far smaller than a classic Wine build root.
#
#     * toolchain + Wine's own generators -- the spritzvalve base ships generators, not
#       their output, so autoconf (configure), perl (tools/make_specfiles) and python3
#       (dlls/winevulkan/make_vulkan) all run during a build.
#     * X11: the exact set the author's own build enables. CI must not ship less.
#     * Wayland + xkbcommon: build.sh documents "GRAPHICAL by default: X11 + Wayland".
#     * freetype: without it Wine has no font backend and dialogs collapse to a few pixels.
#     * alsa + pulseaudio: absent from the old CI package list, so the CI tarball would
#       have shipped without sound. (An earlier claim that the author's LOCAL wine also
#       lacked audio was false -- it came from grepping config.h for SONAME_LIBASOUND,
#       which wine never defines: SONAME_* exists only for dlopen'd libs, and audio is
#       linked directly. smoke.sh judges these backends by the shipped driver .so now.)
#     * udev (systemd-devel) + SDL2, and they are a PAIR. winebus.sys defines
#       is_sdl_ignored_device() inside #ifdef SONAME_LIBSDL2 (bus_sdl.c) but CALLS it
#       unconditionally from bus_udev.c -- which is itself only compiled when libudev is
#       present. So udev WITHOUT SDL2 fails to LINK winebus.so, while configure merrily
#       reports "libSDL2 not found, SDL2 won't be supported" and carries on. Neither the
#       .devcontainer (no udev, no SDL2) nor the author's machine (SDL2, no udev) hits it.
#       We keep both: winebus wants udev for device enumeration, and the soda-glorious tier
#       (DualSense rumble) lives in that path. If you ever drop one, drop both.
#     * image codecs (jpeg/png/tiff/webp): windowscodecs (WIC). From .devcontainer.
#
# (b) The libcosmic GUIs that packaging/package.sh bundles into the tarball
#     (spritzwine-setup, beisl-setup): wayland, xkbcommon, GL/EGL/gbm, vulkan,
#     fontconfig/freetype/expat -- mostly the same packages as (a).
#
# PROVENANCE: .devcontainer/Dockerfile is the closest thing this project has to a canonical
# dependency list -- consult it first when adding to this one. It is not a superset, though:
# it has no udev and no SDL2, and it predates the audio packages, so it is a starting point
# and not an oracle.
RUN dnf install -y --setopt=install_weak_deps=False \
        gcc gcc-c++ make autoconf automake perl-core python3 \
        flex bison patch patchelf file which findutils tar xz git ccache \
        pkgconf-pkg-config clang lld llvm-devel clang-devel glibc-devel \
        libX11-devel libXext-devel libXrandr-devel libXrender-devel libXi-devel \
        libXfixes-devel libXinerama-devel libXcursor-devel libXcomposite-devel \
        libXxf86vm-devel libxcb-devel \
        wayland-devel wayland-protocols-devel libxkbcommon-devel libxkbcommon-x11-devel \
        mesa-libGL-devel mesa-libEGL-devel mesa-libgbm-devel \
        vulkan-loader-devel vulkan-headers \
        freetype-devel fontconfig-devel expat-devel \
        alsa-lib-devel pulseaudio-libs-devel \
        SDL2-devel \
        gstreamer1-devel gstreamer1-plugins-base-devel \
        gnutls-devel krb5-devel openldap-devel cups-devel unixODBC-devel \
        dbus-devel systemd-devel libusb1-devel bluez-libs-devel libxml2-devel libxslt-devel zlib-devel \
        libjpeg-turbo-devel libpng-devel libtiff-devel libwebp-devel \
    && dnf clean all

# Rust, laid out like the official rust image: the toolchain and the shims live in the
# IMAGE, so a job can point CARGO_HOME at the bind-mounted cache (for the registry) without
# losing `cargo` from PATH -- RUSTUP_HOME still resolves the toolchain.
#
# LIBCLANG_PATH is for bindgen (the wine_api crate runs it over Wine's generated headers).
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH \
    LIBCLANG_PATH=/usr/lib64

# The two PE targets are the new-WoW64 bet: the PE-side Rust staticlibs are REAL PE, one
# build per arch, via clang/lld's *-pc-windows-gnullvm targets. No mingw anywhere.
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --no-modify-path --default-toolchain stable --profile minimal \
 && rustup target add i686-pc-windows-gnullvm x86_64-pc-windows-gnullvm \
 && chmod -R a+w "$RUSTUP_HOME" "$CARGO_HOME"

# ccache ahead of the real compilers, so a warm CCACHE_DIR is actually used by Wine's cc.
ENV PATH=/usr/lib64/ccache:$PATH

# The image's own fingerprint (ci/stamp.sh: this Dockerfile with comments stripped, so a
# documentation edit does not invalidate a multi-GB image). Baking it in is what lets a CI
# job compare the image it is RUNNING IN against the commit it is building
# -- force_pull is off and the tag is local, so nothing else can tell a stale image from a
# fresh one (the third identical winebus failure proved that the hard way). The workflows
# assert it, and build.sh reconfigures wine when it changes.
# Keep this the LAST layer (the stamp changes on every edit; everything above stays cached).
# ./ci/rebuild.sh does the stamp+build+verify dance in the right order.
COPY build-env.sha256 /etc/spritzwine/build-env.sha256
