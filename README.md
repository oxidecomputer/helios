# Oxide Helios

Helios is a distribution of illumos powering the Oxide Rack.  The full
distribution is built from several consolidations of software, driven from
tools and documentation in this top-level repository.

| Consolidation                                                                                   | Public? | Description                                                                     |
| ----------------------------------------------------------------------------------------------- | ------- | ------------------------------------------------------------------------------- |
| [boot-image-tools](https://github.com/oxidecomputer/boot-image-tools)                           | ✅ Yes  | Tool for assembling boot images for Oxide hardware                              |
| [garbage-compactor](https://github.com/oxidecomputer/garbage-compactor)                         | ✅ Yes  | Build scripts for packages beyond the core OS                                   |
| [helios-omicron-brand](https://github.com/oxidecomputer/helios-omicron-brand)                   | ✅ Yes  | Zone brand for [Omicron](https://github.com/oxidecomputer/omicron) components   |
| [helios-omnios-build](https://github.com/oxidecomputer/helios-omnios-build)                     | ✅ Yes  | Build scripts for packages beyond the core OS                                   |
| [helios-omnios-extra](https://github.com/oxidecomputer/helios-omnios-extra)                     | ✅ Yes  | Build scripts for packages beyond the core OS                                   |
| [illumos-gate (stlouis branch)](https://github.com/oxidecomputer/illumos-gate/tree/stlouis/)    | ✅ Yes  | Core operating system (kernel, libc, etc)                                       |
| [phbl](https://github.com/oxidecomputer/phbl)                                                   | ✅ Yes  | Pico Host Boot Loader                                                           |
| [pinprick](https://github.com/oxidecomputer/pinprick)                                           | ✅ Yes  | ROM image compression utility                                                   |
| [illumos/image-builder](https://github.com/illumos/image-builder)                               | ✅ Yes  | Tool for building bootable illumos disk images                                  |
| [amd-firmware](https://github.com/oxidecomputer/amd-firmware)                                   | ❌ No   | AMD CPU firmware binary blobs (will be available in future)                     |
| [amd-host-image-builder](https://github.com/oxidecomputer/amd-host-image-builder)               | ❌ No   | ROM image construction tools for AMD CPUs (will be available in future)         |
| [chelsio-t6-roms](https://github.com/oxidecomputer/chelsio-t6-roms)                             | ❌ No   | Chelsio T6 network interface card firmware blobs (will be available in future)  |
| [pilot](https://github.com/oxidecomputer/pilot)                                                 | ❌ No   | A utility for low-level control of Oxide systems (will be available in future)  |

**NOTE:** Not all consolidations are presently available to the public.  We're
working on this, but for now you can set `OXIDE_STAFF=no` in your environment
when you run `gmake setup` to skip cloning and building software that is not
yet available.

## Getting started

**NOTE: These instructions are for building your own operating system packages
and installing them.  If you're just trying to use Helios, you probably do not
need to do this.  See
[helios-engvm](https://github.com/oxidecomputer/helios-engvm) for
information about pre-built Helios software.**

The best way to get started is to be using a physical or virtual build machine
running an up-to-date installation of Helios.  There are some details on
getting a virtual machine installed in the
[helios-engvm](https://github.com/oxidecomputer/helios-engvm) repository.
There are also some details there about install media that you can use on a
physical x86 system.

### Prerequisites

If you used the instructions from **helios-engvm** to create a virtual machine,
you should already have all of the packages needed.  If you used one of the ISO
installers to set up a physical machine, or some other way of getting a Helios
environment, you may need to install the **pkg:/developer/illumos-tools**
package.  You can check if you have this installed already with:

```
$ pkg list developer/illumos-tools
NAME (PUBLISHER)                VERSION    IFO
developer/illumos-tools         11-2.0     im-i
```

If missing from your system, it can be installed with `pkg install`.  It's also
a good idea to be running the latest Helios packages if you can.  You can
update your system with:

```
# pkg update
```

Pay careful attention to the instructions printed at the end of every update.
You may be told that a _boot environment_ was created and that you need to
reboot to activate it.  You should do that with the `reboot` command before
moving on.

### Install Rust and Cargo using Rustup

Official Rust and Cargo binaries are available from the Rust project via the
same [rustup](https://rustup.rs/) tool that works on other systems.  Use the
official instructions, but substitute `bash` anywhere you see `sh`; e.g., at
the time of writing, the (modified) official install instructions are:

```
$ curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | bash
```

### Clone the repository and build the tools

On your Helios machine, clone this repository and run the setup step:

```
$ git clone https://github.com/oxidecomputer/helios.git
Cloning into 'helios'...

$ cd helios
$ gmake setup
cd tools/helios-build && cargo build --quiet
...
Cloning into '/home/user/helios/projects/illumos'...
...

Setup complete!  ./helios-build is now available.
```

**NOTE:** If you do not have access to private repositories in the
**oxidecomputer** GitHub organisation, you can request that the setup step only
use the public repositories; e.g.,

```
$ OXIDE_STAFF=no gmake setup
```

The Rust-based `helios-build` tool will be built and several repositories will
then be cloned under `projects/`.  Note that, at least for now, the tool takes
a little while to build the first time.

While the tool will initially clone the expected project repositories,
subsequent manipulation (e.g., pulling updates, switching branches) is only
performed for some repositories.  You can see which repositories the setup step
will update by looking at `auto_update` in the
[`config/projects.toml`](./config/projects.toml) file.  You should otherwise
expect to manage local clones as you would any other git repository; switching
branches, pulling updates, etc.

## Building illumos

The operating system components at the core of Helios come from the
[**stlouis** branch of
illumos-gate](https://github.com/oxidecomputer/illumos-gate/tree/stlouis).  The
packages that ship on Helios systems are mostly [stock
illumos](https://github.com/illumos/illumos-gate) with some additions for Oxide
hardware and a few minor packaging transformations.

To make it easier to build illumos, `helios-build` provides several wrappers
that manage build configuration and invoke the illumos build tools.  The
upstream illumos documentation has a guide, [Building
illumos](https://illumos.org/docs/developers/build/), which covers most of what
the Helios tools are doing on your behalf if you are curious.

### Building during development

While making changes to illumos, you can perform a "quick" build.  This
disables the shadow compilers and some of the checks that we would otherwise
require for a final integration.

```
$ ./helios-build build-illumos -q
Dec 04 22:04:49.214 INFO file /home/user/helios/projects/illumos/illumos-quick.sh does not exist
Dec 04 22:04:49.215 INFO writing /home/user/helios/projects/illumos/illumos-quick.sh ...
Dec 04 22:04:49.215 INFO ok!
Dec 04 22:04:49.216 INFO exec: ["/sbin/sh", "-c", "cd /home/user/helios/projects/illumos && ./usr/src/tools/scripts/nightly /home/user/helios/projects/illumos/illumos-quick.sh"]
...
```

Depending on how many CPUs you have on your build machine, and the performance
of your local storage, this can take some time.  The full build log is quite
large, and can be seen via, e.g.,

```
$ tail -F projects/illumos/log/nightly.log
```

Once your build has completed successfully, there will be a package repository
at `projects/illumos/packages/i386`.  These packages can then be transformed
and installed in various ways.

#### Installing: locally on your build machine

To install your newly built packages on the build machine:

```
$ ./helios-build onu -t my-be-name
Jan 29 09:33:44.603 INFO creating temporary repository...
...
Jan 29 09:35:53.050 INFO O| beadm activate my-be-name
Jan 29 09:35:53.911 INFO O| Activated successfully
Jan 29 09:35:53.921 INFO onu complete!  you must now reboot
```

This will transform and install the illumos packages you just built and create
a new _Boot Environment_ with the name you pass with `-t` (e.g., `my-be-name`
above).  The new boot environment can be seen with `beadm list`, and has been
activated by `onu` so that you can reboot into it.  See
[beadm(8)](https://illumos.org/man/8/beadm) for more information about boot
environments.

When rebooting, it is a good idea to be on the console so you can see any boot
messages and interact with the boot loader.

```
helios console login: root
Password:
Last login: Mon Jan 29 09:34:20 on console
The illumos Project     stlouis-0-g27e9202a98   January 2024
root@genesis:~# reboot
updating /platform/i86pc/amd64/boot_archive (CPIO)
syncing file systems... done
rebooting...
```

You can see that your updated packages are now running:

```
$ pkg list -Hv system/kernel
pkg://on-nightly/system/kernel@0.5.11-2.0.999999:20240129T090642Z            i--
```

Critically, the `system/kernel` package shown here comes from the `on-nightly`
publisher (your local files) and has a quick build version (`2.0.999999`).

#### Installing: on another machine, using a package repository server

If you have a build machine and a separate set of test machine(s), you may wish
to use the package repository server (`pkg.depotd`) on your build machine.  You
can reconfigure the test system to prefer to install packages from your build
machine over the network without needing to copy files around.

First, transform the packages from your most recent build and start the package
server:

```
$ ./helios-build onu -D
Jan 29 09:39:46.885 INFO creating temporary repository...
Jan 29 09:39:46.886 INFO repository /home/user/helios/tmp/onu/repo.redist exists, removing first
...
Jan 29 09:41:00.428 INFO starting pkg.depotd on packages at: "/home/user/helios/tmp/onu/repo.redist"
Jan 29 09:41:00.428 INFO access log file is "/home/user/helios/tmp/depot/log/access"
Jan 29 09:41:00.428 INFO listening on port 7891
Jan 29 09:41:00.428 INFO ^C to quit
[29/Jan/2024:09:41:01] INDEX Search Available
[29/Jan/2024:09:41:01] ENGINE Listening for SIGTERM.
[29/Jan/2024:09:41:01] ENGINE Listening for SIGHUP.
[29/Jan/2024:09:41:01] ENGINE Listening for SIGUSR1.
[29/Jan/2024:09:41:01] ENGINE Bus STARTING
[29/Jan/2024:09:41:01] ENGINE Serving on http://0.0.0.0:7891
[29/Jan/2024:09:41:01] ENGINE Bus STARTED
```

The server is now running, and will remain running until you press Control-C or
terminate it in some other way.  You will need to know a DNS name or IP address
(e.g., via `ipadm show-addr`) on which your build machine can be contacted.

Now, on the target machine, confirm that you can contact the build machine:

```
$ pkgrepo info -s http://genesis:7891
PUBLISHER  PACKAGES STATUS           UPDATED
on-nightly 549      online           2024-01-29T09:40:50.716102Z
```

Examine your existing package publisher configuration.  On a stock Helios
system, it should look like this:

```
# pkg publisher
PUBLISHER               TYPE   STATUS P LOCATION
helios-dev              origin online F https://pkg.oxide.computer/helios/2/dev/
```

Just one publisher is configured, using the central repository.  We want to add
a second publisher and make it the preferred source for packages.  We also want
to relax the "sticky" rule; i.e., that packages should only be updated from the
publisher from which they were first installed.

```
# pkg set-publisher -r -O http://genesis:7891 --search-first on-nightly
# pkg set-publisher -r --non-sticky helios-dev
# pkg publisher
PUBLISHER               TYPE   STATUS P LOCATION
on-nightly              origin online F http://genesis:7891/
helios-dev (non-sticky) origin online F https://pkg.oxide.computer/helios/2/dev/
```

For now, depending on what you're doing on the test system, it may be necessary
to uninstall the `entire` meta-package before proceeding.  This is especially
true if you have zones based on the `lipkg` brand.  You can do this via `pkg
uninstall entire`.  The stock `onu` tool from illumos does this automatically.

Perform a dry-run update to confirm that we are going to get updated
packages from the quick build on your build machine:

```
# pkg update -nv
            Packages to update:       325
     Estimated space available:  20.67 GB
Estimated space to be consumed: 564.95 MB
       Create boot environment:       Yes
     Activate boot environment:       Yes
Create backup boot environment:        No
          Rebuild boot archive:       Yes

Changed packages:
helios-dev -> on-nightly
  SUNWcs
    0.5.11-2.0.22430 -> 0.5.11-2.0.999999
  SUNWcsd
    0.5.11-2.0.22430 -> 0.5.11-2.0.999999
...
```

Note that the version is changing from a stock Helios version (which is the
commit number on the master branch of illumos) to `2.0.999999`, the quick build
version.  A new boot environment will be created, and a reboot will be
required.

Run the operation again without the `-n` flag to update:

```
# pkg update -v
...
DOWNLOAD                                PKGS         FILES    XFER (MB)   SPEED
Completed                            325/325     5311/5311  107.1/107.1  4.9M/s

PHASE                                          ITEMS
Removing old actions                       1213/1213
Installing new actions                       892/892
Updating modified actions                  5921/5921
Updating package state database                 Done
Updating package cache                       325/325
Updating image state                            Done
Creating fast lookup database                   Done
Reading search index                            Done
Building new search index                    582/582
Updating package cache                           2/2

A clone of helios exists and has been updated and activated.
On the next boot the Boot Environment helios-1 will be
mounted on '/'.  Reboot when ready to switch to this updated BE.

*** Reboot required ***
New BE: helios-1

Updating package cache                           2/2
```

Assuming the update was successful, you should be able to reboot into your
update software!

```
# reboot
updating /platform/i86pc/amd64/boot_archive (CPIO)
```

After reboot, note that the publisher configuration is persistent:

```
Loading unix...
Loading /platform/i86pc/amd64/boot_archive...
Loading /platform/i86pc/amd64/boot_archive.hash...
Booting...
Oxide Helios Version stlouis-0-g27e9202a98 64-bit (onu)
Hostname: helios

helios console login: root
Password:

The illumos Project     stlouis-0-g27e9202a98   January 2024
# uname -v
stlouis-0-g27e9202a98

# pkg publisher
PUBLISHER               TYPE   STATUS P LOCATION
on-nightly              origin online F http://genesis:7891/
helios-dev (non-sticky) origin online F https://pkg.oxide.computer/helios/2/dev/
```

In future, you should be able to do a new build, restart the package server,
and then `pkg update -v` again on the test machine.

#### Installing: producing packages without installing them

If you just want to transform the packages from a quick build without
installing them, you can do so with the `-P` flag:

```
$ ./helios-build onu -P
Jan 29 09:45:36.040 INFO creating temporary repository...
Jan 29 09:45:36.040 INFO repository /home/user/helios/tmp/onu/repo.redist exists, removing first
...
Jan 29 09:46:14.901 INFO O| Republish: pkg:/text/locale@0.5.11,5.11-2.0.999999:20240129T090648Z ...  Done
Jan 29 09:46:15.602 INFO exec: ["/usr/bin/pkgrepo", "refresh", "-s", "/home/user/helios/tmp/onu/repo.redist"], pwd: None
Jan 29 09:46:15.907 INFO O| Initiating repository refresh.
Jan 29 09:46:24.978 INFO transformed packages available for onu at: "/home/user/helios/tmp/onu/repo.redist"
```

This may be useful if you just want to inspect the contents of the built
repository; e.g.,

```
$ pkgrepo info -s tmp/onu/repo.redist
PUBLISHER  PACKAGES STATUS           UPDATED
on-nightly 549      online           2024-01-29T09:46:15.448096Z

$ pkgrepo list -s tmp/onu/repo.redist
PUBLISHER  NAME                          O VERSION
on-nightly SUNWcs                          0.5.11-2.0.999999:20240129T090617Z
on-nightly SUNWcsd                         0.5.11-2.0.999999:20240129T090618Z
on-nightly audio/audio-utilities           0.5.11-2.0.999999:20240129T090618Z
on-nightly benchmark/filebench           o 0.5.11-2.0.999999:20240129T090618Z
...

$ pkg contents -t file -s tmp/onu/repo.redist '*microcode*'
PATH
platform/i86pc/ucode/AuthenticAMD/1020-00
platform/i86pc/ucode/AuthenticAMD/1022-00
platform/i86pc/ucode/AuthenticAMD/1041-00
platform/i86pc/ucode/AuthenticAMD/1043-00
platform/i86pc/ucode/AuthenticAMD/1062-00
platform/i86pc/ucode/AuthenticAMD/1080-00
platform/i86pc/ucode/AuthenticAMD/1081-00
platform/i86pc/ucode/AuthenticAMD/10A0-00
platform/i86pc/ucode/AuthenticAMD/2031-00
...
```

You can also preserve the package files for later analysis such as the
comparison of the output of multiple builds, or transport them to remote
systems for installation.

### Making changes

When making changes to the system it is generally best to start with a pristine
built workspace, as you would have left from the quick build in the previous
section.

Once your build has completed, you may wish to make a change to a particular
source file and rebuild a component.  There are many components in the illumos
repository, but we can choose a simple one as an example here.  To build a
particular component, we must first use `bldenv` to enter the build
environment:

```
$ ./helios-build bldenv -q
Jan 29 09:50:22.895 INFO file /home/user/helios/projects/illumos/illumos-quick.sh exists, with correct contents
Jan 29 09:50:22.895 INFO ok!
Build type   is  non-DEBUG
RELEASE      is
VERSION      is stlouis-0-g27e9202a98
RELEASE_DATE is January 2024

The top-level 'setup' target is available to build headers and tools.

Using /bin/bash as shell.
$ pwd
/home/user/helios/projects/illumos/usr/src
```

A new interactive shell has been started, with `PATH` and other variables set
correctly, and you can now change to a component directory and build it:

```
$ cd cmd/id
$ dmake -S -m serial install
...
```

This will build and install the updated `id` command into the proto area:

```
$ ls -l $ROOT/usr/bin/id
-r-xr-xr-x   1 user     staff      17428 Jan 29 09:51 /home/user/helios/projects/illumos/proto/root_i386-nd/usr/bin/id
```

This kind of targetted incremental edit-and-recompile is a good way to make
changes with a short cycle time and have some expectation that they will
compile.

Once you have changes you want to test, there are various things you can do
next.

#### Option 1: Most correct and slowest

You can always do a new built of the entire OS.  This is the only process that
is (as much as anything can be) guaranteed to produce correct results.  If,
while doing something more incremental, you are experiencing an issue you
cannot explain, a full build is always a good thing to try first.

```
$ ./helios-build build-illumos -q
```

This will rebuild all of illumos and produce packages you can then install
in the usual way, as described in previous sections.

#### Option 2: No guarantees but faster

If you have updated some of the binaries in the proto area (e.g., by
running `dmake install` in a kernel module or a command directory) you may
just be able to regenerate the packages and install them without doing
a full build.

Within `bldenv`, regenerate the packages:

```
$ cd $SRC/pkg
$ dmake install
...
Publishing system-zones-internal to redist repository
Publishing system-test-zfstest to redist repository
Initiating repository refresh.
```

Once you have updated packages you can use them to start a package repository
server or install locally, as described in the previous sections.

#### Option 3: It's your computer

At the end of the day, the operating system is just files in a file system.
The packaging tools and other abstractions often create a kind of mystique
which separates the engineer from this concrete reality -- but you are an adult
and it is your computer!  Other things you can do include:

- Just running the modified binary on the build system, or using `scp` or
  `rsync` to copy it to the test system and run it there.  Sometimes this
  works!  If the binary requires changes to libraries or the kernel, it might
  not work.

- Creating a new boot environment and adjusting the files in it.  Boot
  environments are separate ZFS file systems that can be modified, snapshotted,
  cloned, and booted.  They can be created with `beadm create` and mounted for
  modification with `beadm mount`.  The boot loader allows you to select a
  different boot environment, and you can activate a specific boot environment
  permanently or just for one boot using `beadm activate`.  See the `beadm(1M)`
  manual page for more information.

- Creating a wholly new disk image or ramdisk and booting that in a virtual
  machine or via PXE.  There are some Helios-specific [tools for creating
  images](https://github.com/oxidecomputer/helios-engvm/tree/main/image) that
  can be made to include packages from a quick build, or even just arbitrary
  additional files, by modifying image templates.  These tools are in turn
  based on the upstream
  [illumos/image-builder](https://github.com/illumos/image-builder).

If you want advice on how to do something not completely explained here, or
just to streamline your workflow, please don't hesitate to reach out!

## OS Image Archives

As part of building OS images for Gimlets, an image archive is produced that
includes the boot ROM and the root file system ramdisk image.  It also includes
some metadata in a JSON file, using the same format as [the **omicron1**
brand](https://github.com/oxidecomputer/helios-omicron-brand/) (see **IMAGE
ARCHIVES** in **omicron1(7)**).

The contents of the file represents a committed interface between Helios and
the parts of [Omicron](https://github.com/oxidecomputer/omicron) which need to
download and install OS images on physical systems in the Oxide rack.  The
relevant contents for Omicron usage will always include at least:

| Filename         | Description     |
| ---------------- | --------------- |
| `oxide.json`     | Metadata header file, with at least a `v=1` key and a `t=os` key to identify it as an OS image. |
| `image/rom`      | The host boot ROM image. (32MiB) |
| `image/zfs.img`  | The host root file system ramdisk image. (arbitrary size) |

In addition to the committed files listed above, some additional files may be
present for engineering or diagnostic purposes; e.g., a `unix.z` compressed
kernel, and a `cpio.z` compressed boot archive, for use with **bldb** or
**nanobl-rs**; or an array of extra ROM files with suffixes that represent
different diagnostic capabilities.  Additional files are not committed and may
change at any time in the future.  Software that interprets image archives
should ignore any unrecognised files.

## Licence

Copyright 2024 Oxide Computer Company

Unless otherwise noted, all components are licenced under the [Mozilla Public
License Version 2.0](./LICENSE).
