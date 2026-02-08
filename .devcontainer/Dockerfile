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
    which \
    tldr \
    file \
    python3 \
    python3-pip

RUN dnf install -y \
    glibc-devel.i686 \
    libstdc++-devel.i686 \
    libX11-devel.i686 \
    freetype-devel.i686 \
    freetype.i686 \
    mingw32-freetype.noarch mingw64-freetype.noarch	\
    mingw32-glib2.noarch mingw32-glib2-static.noarch \
    graphite2-devel.i686 brotli-devel.i686 \
    libXrender-devel.i686 \
    gnutls-devel.i686 nettle-devel.i686 libtasn1-devel.i686 libidn2-devel.i686 p11-kit-devel.i686 \
    alsa-lib-devel.i686 \
    bzip2-devel.i686 \
    libpng-devel.i686 \
    harfbuzz-devel.i686 \
    zlib-ng-compat-devel.i686 \
    && dnf clean all

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

# Add i686 target and set as default
RUN rustup target add i686-unknown-linux-gnu
ENV CARGO_BUILD_TARGET=i686-unknown-linux-gnu

# Install mistral-vibe during the Docker build process
RUN pip install mistral-vibe

# Verify Rust installation
RUN rustc --version && cargo --version

# Set LIBCLANG_PATH to help bindgen find libclang
ENV LIBCLANG_PATH=/usr/lib64

WORKDIR /workspace
# RUN chown -R devuser:devuser /workspace

CMD ["tail", "-f", "/dev/null"]
