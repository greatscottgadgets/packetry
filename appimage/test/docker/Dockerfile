FROM debian:10 AS builder
LABEL maintainer="Great Scott Gadgets <dev@greatscottgadgets.com>"
LABEL stage="builder"
LABEL org.opencontainers.image.source=https://github.com/greatscottgadgets/packetry
LABEL org.opencontainers.image.description="GSG Test Image"
LABEL org.opencontainers.image.licenses=BSD-3-Clause

# - setup base system ---------------------------------------------------------

USER root

# add user
RUN useradd -ms /bin/bash runner

# - squish --------------------------------------------------------------------

# FROM debian:10
# COPY --from=builder / /

# See: https://github.com/actions/checkout/issues/1014
#ENV RUNNER="runner"
#ENV HOME="/home/$RUNNER"
ENV RUNNER="root"
ENV HOME="/github/home"

# gtk-4.0
ENV GTK_VERSION="4.14.4"
ENV GTK4="/opt/gtk-$GTK_VERSION"

# bump: 2

USER $RUNNER
WORKDIR $HOME