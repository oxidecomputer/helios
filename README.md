# Oxide Helios

Helios is a distribution of illumos intended to power the Oxide Rack.  The full
distribution is built from several consolidations of software, driven from
tools and documentation in this top-level repository.

## Getting started

**NOTE: These instructions are for building your own operating system packages
and installing them.  If you're just trying to use Helios, you probably do not
need to do this.**

The best way to get started is to be using a physical or virtual build machine
running an up-to-date installation of Helios.  There are some details on
getting a virtual machine installed in the
[helios-engvm](https://github.com/oxidecomputer/helios-engvm.git) repository.

On your Helios machine, clone this repository and run the setup step:

```
$ git clone git@github.com:oxidecomputer/helios.git
Cloning into 'helios'...

$ cd helios
$ gmake setup
cd tools/helios-build && cargo build --quiet
...
Cloning into '/home/user/helios/projects/illumos'...
...

Setup complete!  ./helios-build is now available.
```

The Rust-based `helios-build` tool will be built and several repositories will
then be cloned under `projects/`.  Note that, at least for now, the tool takes
a little while to build the first time.

While the tool will initially clone the expected project repositories, no
subsequent manipulation (e.g., pulling updates) is presently performed.  You
can (and must!) manage the local clones as you would any other git repository;
switching branches, pulling updates, etc.

## Building illumos

The operating system components at the core of Helios come from
[illumos-gate](https://github.com/illumos/illumos-gate).  The packages that
ship on Helios systems are mostly stock illumos with a few minor packaging
transformations.

To make it easier to build illumos, `helios-build` provides several wrappers
that manage build configuration and invoke the illumos build tools.  The
upstream illumos documentation has a guide, [Building
illumos](https://illumos.org/docs/developers/build/), which covers most of what
the Helios tools are doing on your behalf if you are curious.

### Prerequisites

If you've installed the `helios-engvm` listed above, the prerequisites should
already be available. This is the `full` image from that repo. However, if
you're using the `base` image, you'll need to install the
`developer/illumos-tools` package. This includes the closed binaries and
assorted other machinery for `helios-build` to work its magic. You can determine
if you already have this installed with `pkg list developer/illumos-tools.`

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

#### Installing locally

To install your newly built packages on the build machine:

```
$ ./helios-build onu -t my-be-name
Dec 04 22:55:49.470 INFO creating temporary repository...
...
Dec 04 22:58:11.798 INFO O| beadm activate my-be-name
Dec 04 22:58:11.945 INFO O| Activated successfully
Dec 04 22:58:11.994 INFO onu complete!  you must now reboot
```

This will transform and install the illumos packages you just built and create
a new _Boot Environment_ with the name you pass with `-t` (e.g., `my-be-name`
above).  The new boot environment can be seen with `beadm list`, and has been
activated by `onu` so that you can reboot into it.

When rebooting, it is a good idea to be on the console so you can see any boot
messages and interact with the boot loader.

```
helios console login: root
Password:
Dec  4 22:58:11 helios login: ROOT LOGIN /dev/console
The illumos Project     master-0-g7b4214534c    December 2020
# reboot
Dec  4 22:58:49 helios reboot: initiated by root on /dev/console
updating /platform/i86pc/amd64/boot_archive (CPIO)
syncing file systems... done
rebooting...
```

You can see that your updated packages are now running:

```
$ pkg list -Hv SUNWcs
pkg://on-nightly/SUNWcs@0.5.11-1.0.999999:20201204T223805Z                   i--
```

Critically, the `SUNWcs` package shown here comes from the `on-nightly`
publisher (your local files) and has a quick build version (`1.0.999999`).

#### Running a package repository server and installing somewhere else

If you have a build machine and a separate set of test machine(s), you may wish
to use the package repository server (`pkg.depotd`) on your build machine.  You
can reconfigure the test system to prefer to install packages from your build
machine over the network without needing to copy files around.

First, transform the packages from your most recent build and start the package
server:

```
$ ./helios-build onu -D
Sep 23 14:13:13.414 INFO creating temporary repository...
Sep 23 14:13:13.415 INFO repository /ws/helios/tmp/onu/repo.redist exists, removing first
...
Sep 23 14:14:31.315 INFO starting pkg.depotd on packages at: "/ws/helios/tmp/onu/repo.redist"
Sep 23 14:14:31.316 INFO access log file is "/ws/helios/tmp/depot/log/access"
Sep 23 14:14:31.316 INFO listening on port 7891
Sep 23 14:14:31.316 INFO ^C to quit
[23/Sep/2021:14:14:31] INDEX Search Available
[23/Sep/2021:14:14:31] ENGINE Listening for SIGTERM.
[23/Sep/2021:14:14:31] ENGINE Listening for SIGHUP.
[23/Sep/2021:14:14:31] ENGINE Listening for SIGUSR1.
[23/Sep/2021:14:14:31] ENGINE Bus STARTING
[23/Sep/2021:14:14:31] ENGINE Serving on http://0.0.0.0:7891
[23/Sep/2021:14:14:31] ENGINE Bus STARTED
```

The server is now running, and will remain running until you press Control-C or
terminate it in some other way.  You will need to know a DNS name or IP address
(e.g., via `ipadm show-addr`) on which your build machine can be contacted.

Now, on the target machine, confirm that you can contact the build machine:

```
# pkgrepo info -s http://vulcan:7891
PUBLISHER  PACKAGES STATUS           UPDATED
on-nightly 532      online           2021-09-23T21:44:29.616498Z
```

Examine your existing package publisher configuration.  On a stock Helios
system, it should look like this:

```
# pkg publisher
PUBLISHER               TYPE     STATUS P LOCATION
helios-dev              origin   online F https://pkg.oxide.computer/helios-dev/
```

Just one publisher is configured, using the central repository.  We want to add
a second publisher and make it the preferred source for packages.  We also want
to relax the "sticky" rule; i.e., that packages should only be updated from the
publisher from which they were first installed.

```
# pkg set-publisher -r -O http://vulcan:7891 --search-first on-nightly
# pkg set-publisher -r --non-sticky helios-dev
# pkg publisher
PUBLISHER               TYPE     STATUS P LOCATION
on-nightly              origin   online F http://vulcan:7891/
helios-dev (non-sticky) origin   online F https://pkg.oxide.computer/helios-dev/
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
    0.5.11-1.0.20664 -> 0.5.11-1.0.999999
  SUNWcsd
    0.5.11-1.0.20664 -> 0.5.11-1.0.999999
...
```

Note that the version is changing from a stock Helios version (which is the
commit number on the master branch of illumos) to `1.0.999999`, the quick build
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

A clone of 1203700f exists and has been updated and activated.
On the next boot the Boot Environment 1203700f-1 will be
mounted on '/'.  Reboot when ready to switch to this updated BE.

*** Reboot required ***
New BE: 1203700f-1

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
Oxide Helios Version master-0-g0915aaef57 64-bit (onu)
Hostname: helios

helios console login: root
Password:

The illumos Project     master-0-g0915aaef57   September 2021
# uname -v
master-0-g0915aaef57

# pkg publisher
PUBLISHER               TYPE     STATUS P LOCATION
on-nightly              origin   online F http://vulcan:7891/
helios-dev (non-sticky) origin   online F https://pkg.oxide.computer/helios-dev/
```

In future, you should be able to do a new build, restart the package server,
and then `pkg update -v` again on the test machine.

#### Producing packages without installing

If you just want to transform the packages from a quick build without
installing them, you can do so with the `-P` flag:

```
$ ./helios-build onu -P
Sep 23 15:27:56.254 INFO creating temporary repository...
Sep 23 15:27:56.255 INFO repository /ws/helios/tmp/onu/repo.redist exists, removing first
...
Sep 23 15:28:35.964 INFO O| Republish: pkg:/text/locale@0.5.11,5.11-1.0.999999:20210914T044939Z ...  Done
Sep 23 15:28:36.775 INFO exec: ["/usr/bin/pkgrepo", "refresh", "-s", "/ws/helios/tmp/onu/repo.redist"]
Sep 23 15:28:37.096 INFO O| Initiating repository refresh.
Sep 23 15:28:48.434 INFO transformed packages available for onu at: "/ws/helios/tmp/onu/repo.redist"
```

This may be useful if you just want to inspect the contents of the built
repository; e.g.,

```
$ pkgrepo info -s tmp/onu/repo.redist
PUBLISHER  PACKAGES STATUS           UPDATED
on-nightly 532      online           2021-09-23T22:28:36.597380Z

$ pkgrepo list -s tmp/onu/repo.redist
PUBLISHER  NAME                          O VERSION
on-nightly SUNWcs                          0.5.11-1.0.999999:20210914T044859Z
on-nightly SUNWcsd                         0.5.11-1.0.999999:20210914T044859Z
on-nightly audio/audio-utilities           0.5.11-1.0.999999:20210914T044901Z
on-nightly benchmark/filebench           o 0.5.11-1.0.999999:20210914T044901Z
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
platform/i86pc/ucode/AuthenticAMD/3010-00
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
Sep 23 15:36:25.353 INFO file /ws/helios/projects/illumos/illumos-quick.sh exists, with correct contents
Sep 23 15:36:25.354 INFO ok!
Build type   is  non-DEBUG
RELEASE      is
VERSION      is master-0-g0915aaef57
RELEASE_DATE is September 2021

The top-level 'setup' target is available to build headers and tools.

Using /bin/bash as shell.
$ pwd
/ws/helios/projects/illumos/usr/src
```

A new interactive shell has been started, with `PATH` and other variables set
correctly, and you can now change to a component directory and build it:

```
$ cd cmd/id
$ dmake -m serial install
...
```

This will build and install the updated `id` command into the proto area:

```
$ ls -l $ROOT/usr/bin/id
-r-xr-xr-x   1 jclulow  staff      17688 Sep 23 15:38 /ws/helios/projects/illumos/proto/root_i386-nd/usr/bin/id
```

This kind of targetted incremental edit-and-recompile is a good way to make
changes with a short cycle time and have some expectation that they will
compile.

Once you have changes you want to test, there are various things you can do
next.

#### Most correct and slowest

You can always do a new built of the entire OS.  This is the only process that
is (as much as anything can be) guaranteed to produce correct results.  If,
while doing something more incremental, you are experiencing an issue you
cannot explain, a full build is always a good thing to try first.

```
$ ./helios-build build-illumos -q
```

This will rebuild all of illumos and produce packages you can then install
in the usual way, as described in previous sections.

#### No guarantees but faster

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

#### It's your computer

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
