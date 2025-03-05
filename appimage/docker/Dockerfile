ARG GTK_VERSION_DEFAULT="4.14.4"
ARG NODE_VERSION_DEFAULT="v20.15.1"

FROM debian:10 AS builder
LABEL org.opencontainers.image.source=https://github.com/greatscottgadgets/packetry
LABEL org.opencontainers.image.description="GSG Packetry AppImage Builder"
LABEL org.opencontainers.image.licenses=BSD-3-Clause
LABEL maintainer="Great Scott Gadgets <dev@greatscottgadgets.com>"
LABEL stage="builder"

ARG GTK_VERSION_DEFAULT
ARG NODE_VERSION_DEFAULT
ENV GTK_VERSION="$GTK_VERSION_DEFAULT"
ENV NODE_VERSION="$NODE_VERSION_DEFAULT"

ENV RUNNER="runner"
ENV HOME="/home/$RUNNER"

# - setup base system ---------------------------------------------------------

USER root

# add image dependencies
RUN apt update && apt upgrade -y && apt install -y \
    bison \
    build-essential \
    cmake \
    curl \
    desktop-file-utils \
    dos2unix \
    fdupes \
    flex \
    git \
    libbz2-dev \
    libdrm-dev \
    libepoxy-dev \
    libffi-dev \
    libgstreamer-plugins-bad1.0-dev \
    liblzma-dev \
    libncursesw5-dev \
    libreadline-dev \
    libsqlite3-dev \
    libssl-dev \
    libwayland-bin \
    libwayland-dev \
    libxcb-xkb-dev \
    libxcursor-dev \
    libxdamage-dev \
    libxfixes-dev \
    libxi-dev \
    libxinerama-dev \
    libxkbcommon-dev \
    libxkbcommon-x11-dev \
    libxml2-dev \
    libxml2-utils \
    libxmlsec1-dev \
    libxrandr-dev \
    libxrender-dev \
    tk-dev \
    xvfb \
    xz-utils \
    zlib1g-dev

# remove libharfbuzz0b
RUN apt remove -y libharfbuzz0b

# add user
RUN useradd -ms /bin/bash runner


# - tools ---------------------------------------------------------------------

USER root

# appimagetool, linuxdeploy, linuxdeploy-plugin-gtk
ADD https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage /usr/bin
ADD https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/master/linuxdeploy-plugin-gtk.sh /usr/bin
ADD https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage /usr/bin
RUN chmod 0755 /usr/bin/appimagetool-x86_64.AppImage \
               /usr/bin/linuxdeploy-x86_64.AppImage  \
               /usr/bin/linuxdeploy-plugin-gtk.sh


# nodejs for Swatinem/rust-cache@v2
USER $RUNNER
WORKDIR $HOME

RUN curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.39.7/install.sh | bash
RUN /bin/bash -c "source $HOME/.nvm/nvm.sh && nvm install $NODE_VERSION && nvm use --delete-prefix $NODE_VERSION"


# pyenv
USER $RUNNER
WORKDIR $HOME

ENV PYENV_ROOT="$HOME/.pyenv"
ENV PATH="$PYENV_ROOT/shims:$PYENV_ROOT/bin:$PATH"

RUN git clone https://github.com/pyenv/pyenv.git ~/.pyenv
RUN pyenv install 3.8 && pyenv global 3.8 && pyenv rehash && pip install --upgrade pip


# rust
USER $RUNNER
WORKDIR $HOME

# We could cut half a gig from the image if the GitHub Action could do this instead:
#
#  ... | sh -s -- --default-toolchain none -y
#
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- --profile minimal -y


# - setup gtk-4 build ---------------------------------------------------------

USER $RUNNER
WORKDIR $HOME

ENV GTK_SOURCE="$HOME/gtk-$GTK_VERSION"

# install meson & ninja
RUN pip install meson ninja

# download gtk-4 sources
RUN A=`echo $GTK_VERSION | cut -d. -f1,2` B=$GTK_VERSION && \
  curl -L https://download.gnome.org/sources/gtk/$A/gtk-$B.tar.xz | tar xJf - -C .

# setup gtk-4 build
WORKDIR $GTK_SOURCE

# patch gtk-4 to use specific subproject releases rather than main
ADD patches/gtk-4.14.4-pin-fribidi.patch .
RUN patch -p1 < ./gtk-4.14.4-pin-fribidi.patch
ADD patches/gtk-4.14.4-pin-gi-docgen.patch .
RUN patch -p1 < ./gtk-4.14.4-pin-gi-docgen.patch
ADD patches/gtk-4.14.4-pin-libsass.patch .
RUN dos2unix ./subprojects/libsass.wrap # buster's patch does not support --ignore-white-space
RUN patch -p1 < ./gtk-4.14.4-pin-libsass.patch
ADD patches/gtk-4.14.4-pin-pango.patch .
RUN patch -p1 < ./gtk-4.14.4-pin-pango.patch
ADD patches/gtk-4.14.4-pin-sysprof.patch .
RUN patch -p1 < ./gtk-4.14.4-pin-sysprof.patch

RUN meson setup --prefix /opt/gtk-$GTK_VERSION builddir \
        -Dbuildtype=release \
        -Dmedia-gstreamer=disabled \
        -Dprint-cups=disabled \
        -Dcloudproviders=disabled \
        -Dcolord=disabled \
        -Dvulkan=disabled \
        -Dx11-backend=true \
        -Dwayland-backend=false \
        -Dbuild-testsuite=false \
        -Dbuild-tests=false \
        -Dbuild-examples=false \
        -Dbuild-demos=false \
        -Dintrospection=enabled

# patch glibc for gcc < 9.1
ADD patches/gtk-4.14.4-gcc-lt-9.patch .
RUN patch -p1 < ./gtk-4.14.4-gcc-lt-9.patch


# - build gtk-4 ---------------------------------------------------------------

USER $RUNNER
WORKDIR $GTK_SOURCE/builddir

RUN ninja


# - install gtk-4 -------------------------------------------------------------

USER root
WORKDIR $GTK_SOURCE/builddir

RUN ninja install


# - cleanup -------------------------------------------------------------------

USER $RUNNER
WORKDIR $HOME

RUN rm -rf \
  .cache \
  gtk-$GTK_VERSION


# - squish --------------------------------------------------------------------

FROM debian:10
LABEL org.opencontainers.image.source=https://github.com/greatscottgadgets/packetry
LABEL org.opencontainers.image.description="GSG Packetry AppImage Builder"
LABEL org.opencontainers.image.licenses=BSD-3-Clause
LABEL maintainer="Great Scott Gadgets <dev@greatscottgadgets.com>"
LABEL stage="squish"

COPY --from=builder / /

ARG GTK_VERSION_DEFAULT
ARG NODE_VERSION_DEFAULT
ENV GTK_VERSION="$GTK_VERSION_DEFAULT"
ENV NODE_VERSION="$NODE_VERSION_DEFAULT"


# - setup environment ---------------------------------------------------------
#
# GitHub Actions only supports containers running as root
# See: https://github.com/actions/checkout/issues/1014

ENV RUNNER="root"
ENV HOME="/github/home"

# Assuage GitHub Actions by setting root user home directory to /github/home
RUN sed -i'' 's@root:x:0:0:root:/root:/bin/bash@root:x:0:0:root:/github/home:/bin/bash@g' /etc/passwd

USER $RUNNER
WORKDIR $HOME

# appimagetool, linuxdeploy, linuxdeploy-plugin-gtk
ENV APPIMAGE_EXTRACT_AND_RUN="1"

# nodejs for Swatinem/rust-cache@v2
ENV NVM_DIR="$HOME/.nvm"
ENV NODE_PATH="$NVM_DIR/versions/node/$NODE_VERSION/bin"
ENV PATH="$NVM_DIR:$NODE_PATH:$PATH"

# pyenv
ENV PYENV_ROOT="/home/runner/.pyenv"
ENV PATH="$PYENV_ROOT/shims:$PYENV_ROOT/bin:$PATH"

# rust
ENV PATH="$HOME/.cargo/bin:$PATH"

# gtk-4.0
ENV GTK4="/opt/gtk-$GTK_VERSION"

ENV CPPFLAGS="-I$GTK4/include"
ENV LDFLAGS="-L$GTK4/lib/x86_64-linux-gnu"
ENV PKG_CONFIG_PATH="$GTK4/lib/x86_64-linux-gnu/pkgconfig"
ENV LD_LIBRARY_PATH="$GTK4/lib/x86_64-linux-gnu"
ENV PATH="$GTK4/bin:$PATH"
