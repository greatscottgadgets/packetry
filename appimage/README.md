# Linux AppImage Workflow

This directory contains the files used by the [`appimage.yml`](../.github/workflows/appimage.yml) CI workflow.

There are two stages to the workflow:

**Stage 1:** Create a docker image that includes the rust compiler and any native dependencies required to compile packetry.

**Stage 2:** Use the image created in the first stage to compile packetry and generate the Linux AppImage file.


## Docker Image Maintenance

The Docker image is created from the appimage [`Dockerfile`](docker/Dockerfile).

### Arguments

```
ARG GTK_VERSION_DEFAULT="4.14.4"
```

Setting this argument will allow you to target different versions of gtk4 used by the image.

> **Note:** using other versions will require some modifications further in the Dockerfile as a fair amount of intervention may be required to build it on older systems.

```
ARG NODE_VERSION_DEFAULT="v20.15.1"
```

The `Swatinem/rust-cache@2` workflow action used by the workflow requires an up-to-date release of nodejs which is not available as a package for this Debian release.

Modifying this will allow you to install a different version of nodejs.

### Image base

```
FROM debian:10 AS builder
```

[AppImage best practice](https://docs.appimage.org/introduction/concepts.html#build-on-old-systems-run-on-newer-systems) is to compile applications on the oldest still-supported Linux distribution that we can assume users to still use.

For Linux AppImage builds we are using Debian 10 ("Buster") which was released on 6 July 2019 and exited LTS on 30 June 2024. This is currently the oldest Debian release that can build the latest gtk4 release.

The image itself is based off the official [`debian:10`](https://hub.docker.com/_/debian) image. Other versions can be explored by simply setting the tag to the version required.

> **Note:** Whenever bumping either the gtk source release version or the version of debian it's important to double check that there are _no_ debian packages containing gtk4 libraries installed on the system as the gtk4 build-system will prefer them over compiling newer versions from the source code distribution.
>
> If the debian packages are used this can cause both runtime problems as well as conflicts between the gtk4 licenses we retrieve manually and the licenses that ship with the distribution.
>
> At the time the only package that needs pruning is `libharfbuzz`, which is done in the `Dockerfile` after installing the base set of dependencies.


### Metadata

```
LABEL org.opencontainers.image.source=https://github.com/greatscottgadgets/packetry
LABEL org.opencontainers.image.description="GSG Packetry AppImage Builder"
LABEL org.opencontainers.image.licenses=BSD-3-Clause
LABEL maintainer="Great Scott Gadgets <dev@greatscottgadgets.com>"
LABEL stage="builder"
```

Most of these are self-explanatory but two of the labels are quite important:

The `stage` label is useful because it can be used to clear the cache during development with:

    docker builder prune --filter "cache=true" --filter "label=key=value"

The `org.opencontainers.image.source` label is used by GitHub Packages to link Docker images published to the [ghcri.io](https://ghcr.io) registry back to the source repository. Without this label the `docker/build-push-action` will not have permission to push to the container registry.


### Dependencies

#### AppImage tools

These files are used to build the AppImage redistributable and are downloaded, at build-time, from their latest releases:

* [`appimagetool`](https://github.com/AppImage/appimagetool)
* [`linuxdeploy`](https://github.com/linuxdeploy/linuxdeploy)
* [`linuxdeply-plugin-gtk`](https://github.com/linuxdeploy/linuxdeploy-plugin-gtk)


#### nodejs

The Dockerfile will install the version of nodejs specified by the `NODE_VERSION_DEFAULT` argument using the `v0.39.7` release of the [`nvm`](https://github.com/nvm-sh/nvm) installer.

Alternative versions of the installer can be used by modifying the version in:

    RUN curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.39.7/install.sh | bash


#### pyenv

The gtk4 build system makes extensive use of Python. We use the latest Python 3.8 release as this is the minimum supported version for the company as a whole.


#### gtk4

Packetry relies on a recent release of GTK4 (4.14.4 at the time of writing) which is not available as an installable package. This requires that it be built from source.

We use a minimal build configuration as many of the options are not available on older systems:

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

The effect of these flags can vary wildly between gtk releases so this may require some fiddling if updating to a newer gtk version.

For the 4.14.4 release a number of small [patches](docker/patches/) need to be applied after configuration which are unlikely to have the desired effect on other releases:

   * `gtk-4.14.4-gcc-lt-9.patch`      - fixes build for gcc < 9.1
   * `gtk-4.14.4-pin-fribidi.patch`   - pins fribidi to a specific release.
   * `gtk-4.14.4-pin-gi-docgen.patch` - pins gi-docgen to a specific release.
   * `gtk-4.14.4-pin-libsass.patch`   - pins libsass to a specific release.
   * `gtk-4.14.4-pin-pango.patch`     - pins pango to a specific release.
   * `gtk-4.14.4-pin-sysprof.patch`   - pins sysprof to a specific release.

Finally, gtk4 is installed to the /opt/gtk-$GTK_VERSION directory. It's quite important that it _not_ be installed to `/usr` as gtk4 ships with newer versions of some system libraries which can horribly confuse both the Rust compiler and the AppImage tools.

### Squish

Once the `builder` build is completed the image is squashed into a single layer to reduce registry storage space.


### Setup Environment

Once the image has been squashed the environment is configured for use by the workflow.

#### Root user configuration

```
ENV RUNNER="root"
ENV HOME="/github/home"

# Assuage GitHub Actions by setting root user home directory to /github/home
RUN sed -i'' 's@root:x:0:0:root:/root:/bin/bash@root:x:0:0:root:/github/home:/bin/bash@g' /etc/passwd

USER $RUNNER
WORKDIR $HOME
```

GitHub not only has some [difficulties](https://github.com/actions/checkout/issues/1014) operating within a container as a non-root user but it will also override the `$HOME` environment variable with a different location to the one specified for `root` in the password file.

This causes endless difficulties that require some ugly hacks.

The first is that we need to modify the `/etc/passwd` file to reflect the home directory location GitHub expects:

    root:x:0:0:root:/github/home:/bin/bash

The rest of the steps required to make all this work live in the [`appimage.yml`](../.github/workflows/appimage.yml) CI workflow.

#### Set Variables

Finally environment variables are set for the tools and gtk4 installation.

While one may be tempted to wonder why the `pyenv` installation still points to the `/home/runner` directory instead of `$HOME` I would encourage everyone to withstand the temptation to change it.


### Notes

The image naming convention used is `<org or user>/<repo>-<variant>:<branch>`.

For example:

    greatscottgadgets/packetry-gtk-4.14.4:main

Docker images are published with `private` visibility and can be viewed in the [GSG Packages](https://github.com/orgs/greatscottgadgets/packages) tab.

Over time the image may start taking up some space because a new tag is built for every branch and PR.

There are two ways this could be handled:

1. Simply delete the entire package. It will always be recreated the next time the workflow runs.
2. Potentially write a workflow to delete tags once a PR is merged or a branch deleted.


## Workflow Maintenance

The Linux AppImage workflow lives inside the [`.github/workflows/appimage.yml`](../.github/workflows/appimage.yml) file.

### Job: Create Docker Image

#### Permissions

Publishing docker images to the ghcr.io registry requires a few permissions:

    permissions:
      contents: read       # read the repo
      packages: write      # read & publish docker image to the registry
      attestations: write  # read & publish docker image attestation to the registry
      id-token: write      # let the repository know it's github

A consequence of this is this workflow will _fail_ on any PR's unless they originate from the main repository. GitHub will remove the `write` authorization and only provide `read` access.

This means that if you want to make any changes to the Dockerfile you need to work off a branch from the main repo and not a fork!

### Job: Build Linux AppImage

#### Step: Permissions

The permission set is minimal:

    permissions:
      contents: read  # read the repo
      packages: read  # read docker images from the registry

#### Step: Root user workaround

```
- name: Workaround issues when GitHub does not respect the HOME env var
  run: |
    mv /home/runner/.cargo   $HOME
    mv /home/runner/.nvm     $HOME
    mv /home/runner/.rustup  $HOME
```

If, in future, any other user tools are installed in the image this may need to be updated!

#### Step: Gather licenses

The licences for the static libraries linked into the packetry binary are obtained by using the [`../wix/rust_licenses.py`](../wix/rust_licenses.py) tool.

Once gathered they are copied to the default licence files location for AppImage in [`packetry.AppDir/usr/share/doc/`](packetry.AppDir/usr/share/doc/).

#### Step: Run build appimage action

The final step calls a custom action that will combine the output of our previous steps into an AppImage binary.


## AppImage Custom Action Maintenance

The appimage custom action lives inside the [`action.yml`](action.yml) file.

#### Shrink packetry-x86_64.AppDir

The [`linuxdeply-plugin-gtk`](https://github.com/linuxdeploy/linuxdeploy-plugin-gtk) plugin does a good job of making sure everything we need to run a gtk app is bundled but we can still reduce the image size significantly:

1. Remove any system libraries we don't actually want to distribute as they could cause conflicts if users have different versions installed.
2. The plugin, for some reason, dereferences the library files when copying them over resulting in a massive size increase to the binary. Ordinarily one would just delete the dupes and run ldconfig but, for reasons (*'file is truncated'* - don't ask!), this doesn't work with the gtk libraries. So we do a simple little bash script instead to delete dupes and replace them with symlinks.
3. Finally, all libraries are stripped of all debug information.


#### Gather licenses

This is not a particularly elegant solution. The [`linuxdeploy`](https://github.com/linuxdeploy/linuxdeploy) tool does a reasonable job of scanning through the packages used by the AppImage with `dpkg` and obtaining the license files.

Unfortunately this only works for the system libraries as the gtk4 libraries are installed from source.

The current state of affairs involves a manual step of tracking down an upstream Debian-based release that has packaged the version of gtk4 we are building and then manually grabbing the machine-readable debian license files from it. These files are then used to pre-populate the [`appimage/packetry.AppDir/usr/share/doc/`](packetry.AppDir/usr/share/doc/) directory in the git repository.

A better solution is possible but it is complicated by rather obtuse package query capabilities available to us.

That said, a rough sketch of an automated solution would look something like this:

1. Identify a distribution with gtk4 packages that are relatively close to the version of gtk4 we're using.
2. Make a list (probably manually) of the package names that map to the gtk4 libraries we've built.
3. Manually downloading each package and extracting the `copyright` file.

So probably not something one would want to do at CI time but it could make maintenance of the pre-populated licenses easier.

#### Convert icon to application/x-executable on all themes (Linux)

Annoyingly AppImage does not allow you to refrain from setting an icon for your AppImage. (This has been confirmed by the author to be by design)

In the absence of an official packetry icon a simple workaround is to simply truncate the icon file to zero bytes before generating the final image.
