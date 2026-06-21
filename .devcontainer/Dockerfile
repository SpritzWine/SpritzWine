FROM fedora:44

# -------------------------------------------------
# SECTION 1: Install General Development Dependencies
# -------------------------------------------------

RUN dnf update -y && dnf clean all
# RUN dnf builddep wine -y
RUN dnf install -y \
    gcc \
    make \
    git \
    wget \
    curl \
    glibc-devel \
    libX11-devel \
    libXext-devel \
    libXi-devel \
    libXrender-devel \
    libXrandr-devel \
    libXcursor-devel \
    libXcomposite-devel \
    libXdamage-devel \
    libXfixes-devel \
    libXxf86vm-devel \
    libXinerama-devel \
    libXmu-devel \
    libXtst-devel \
    libXv-devel \
    libXScrnSaver-devel \
    libxkbfile-devel \
    libxml2-devel \
    libxslt-devel \
    mingw-w64-tools.x86_64 \
    flex \
    bison \
    cups-devel \
    gnutls-devel \
    freetype-devel.x86_64 freetype.x86_64 \
    mingw64-glib2.noarch mingw64-glib2-static.noarch \
    libjpeg-turbo-devel \
    libpng-devel \
    libtiff-devel \
    libwebp-devel \
    alsa-lib-devel \
    openldap-devel \
    openssl-devel \
    samba-devel \
    sqlite-devel \
    ninja-build \
    meson \
    llvm llvm-devel \
    clang clang-devel \
    lld \
    patchelf \
    xz \
    which \
    tldr \
    file \
    python3 \
    python3-pip

# NOTE: new-WoW64 builds real PE (clang+lld) over a single 64-bit Unix side, so it
# needs NO 32-bit (.i686) ELF dev packages -- the old 32-bit dependency set that used
# to live here is gone, along with the 32-bit Wayland/pkg-config dance.

# -------------------------------------------------
# SECTION 2: Install Rust
# -------------------------------------------------
RUN groupadd --gid 1000 devuser \
    && useradd --uid 1000 --gid devuser --shell /bin/bash --create-home devuser \
    && echo "devuser ALL=(root) NOPASSWD:ALL" >> /etc/sudoers

USER devuser

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
ENV PATH="/home/devuser/.cargo/bin:${PATH}"

# Ensure pip is available for installing Python packages
ENV PATH="${PATH}:/usr/bin/python3"

# Rust targets for new-WoW64: x86_64-unknown-linux-gnu (the host default) carries the
# Unix-side cdylibs; the two PE targets carry the PE-side staticlib. No
# CARGO_BUILD_TARGET override -- rust/.cargo/config.toml pins the host triple, and an
# env override here would force the wrong target.
RUN rustup target add i686-pc-windows-gnullvm x86_64-pc-windows-gnullvm

# Install mistral-vibe during the Docker build process
RUN pip install mistral-vibe

# Verify Rust installation
RUN rustc --version && cargo --version

# Set LIBCLANG_PATH to help bindgen find libclang
ENV LIBCLANG_PATH=/usr/lib64

WORKDIR /workspace
# RUN chown -R devuser:devuser /workspace

CMD ["tail", "-f", "/dev/null"]
