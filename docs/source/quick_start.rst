=============================
Getting Started with Packetry
=============================

Install Cynthion
----------------

Before installing Packetry, follow the instructions to `install Cynthion host software <https://cynthion.readthedocs.io/en/latest/getting_started.html>`__ and then ensure that your Cynthion is up to date by running:

.. code::

   cynthion update


Install Packetry
----------------

.. tab:: Linux

    .. tab:: Binary

        AppImage coming soon! For now you can build and install with Cargo.

    .. tab:: Cargo

        **Install Prerequisites**

        Install the Rust build tools, other essential build tools, and GTK 4 headers:

        .. tab:: Debian/Ubuntu

            .. code::

                apt install rustc cargo build-essential libgtk-4-dev git

        .. tab:: Fedora

            .. code::

                yum install rust cargo make gcc gcc-c++ gtk4-devel pango-devel git

        Note that Packetry requires a minimum Rust version of 1.74. If your distribution's packages are older than this, use `rustup <https://rustup.rs/>`__ to get the latest Rust toolchain and manage your Rust installation.

        **Install Packetry**

        Clone the Packetry repository:

        .. code::

            git clone https://github.com/greatscottgadgets/packetry.git

        **Build Packetry**

        .. code::

            cd packetry
            cargo build

        Note: Do not build with ``--all-features``. All the optional features currently in the package are for debug/test purposes only, and will prevent or degrade normal use of the application if enabled. See ``Cargo.toml`` for details.

.. tab:: macOS

    .. tab:: Binary

        Homebrew installation coming soon!

    .. tab:: Cargo

        **Install Prerequisites**

        Use `rustup <https://rustup.rs/>`__ to install the latest Rust toolchain and manage your Rust installation.

        Use `Homebrew <https://brew.sh/>`__ to install GTK 4:

        .. code::

            brew install gtk4

        **Install Packetry**

        Clone the Packetry repository:

        .. code::

            git clone https://github.com/greatscottgadgets/packetry.git

        **Build Packetry**

        .. code::

            cd packetry
            cargo build

        Note: Do not build with ``--all-features``. All the optional features currently in the package are for debug/test purposes only, and will prevent or degrade normal use of the application if enabled. See ``Cargo.toml`` for details.

.. tab:: Windows

   Download and run the Windows installer from the `latest release <https://github.com/greatscottgadgets/packetry/releases/latest>`__.


Connect Cynthion
----------------

Connect Cynthion's CONTROL port to your analysis host computer running Packetry. Connect TARGET C to your target host computer. If your target host and analysis host are the same computer, please be aware of `this issue <https://github.com/greatscottgadgets/packetry/issues/122>`__.

You can connect Cynthion's TARGET A port to your target device at this time, but you may wish to delay this connection until after Packetry is capturing. This will ensure that Packetry can observe the entire device enumeration process.

.. image:: ../images/cynthion-connections-packetry.svg
  :width: 800
  :alt: Connection diagram for using Cynthion with Packetry.


Launch Packetry
---------------

.. tab:: Binary

   Run the Packetry executable.

.. tab:: Cargo

   In the ``packetry`` directory run:

    .. code::

        cargo run

The Packetry window should appear.


Start Capturing USB Traffic
---------------------------

Make sure that "Cynthion" is selected in the Device Drop-down. If this list is empty, use the Scan Button to search for a capture device.

Select the target device speed from the Speed Drop-down. If you don't know the target speed, try each speed until you find the one that works for your device. (Automatic speed detection coming soon!)

Click the Capture Button.

If the target device is not already connected to TARGET A, connect it now.
