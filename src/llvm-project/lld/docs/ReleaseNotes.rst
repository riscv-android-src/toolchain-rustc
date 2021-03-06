========================
lld 13.0.0 Release Notes
========================

.. contents::
    :local:

.. warning::
   These are in-progress notes for the upcoming LLVM 13.0.0 release.
   Release notes for previous releases can be found on
   `the Download Page <https://releases.llvm.org/download.html>`_.

Introduction
============

This document contains the release notes for the lld linker, release 13.0.0.
Here we describe the status of lld, including major improvements
from the previous release. All lld releases may be downloaded
from the `LLVM releases web site <https://llvm.org/releases/>`_.

Non-comprehensive list of changes in this release
=================================================

ELF Improvements
----------------

* ``-Bsymbolic -Bsymbolic-functions`` has been changed to behave the same as ``-Bsymbolic-functions``. This matches GNU ld.
  (`D102461 <https://reviews.llvm.org/D102461>`_)
* ``-Bno-symbolic`` has been added.
  (`D102461 <https://reviews.llvm.org/D102461>`_)
* A new linker script command ``OVERWRITE_SECTIONS`` has been added.
  (`D103303 <https://reviews.llvm.org/D103303>`_)
* ``-Bsymbolic-non-weak-functions`` has been added as a ``STB_GLOBAL`` subset of ``-Bsymbolic-functions``.
  (`D102570 <https://reviews.llvm.org/D102570>`_)

Breaking changes
----------------

* ``--shuffle-sections=<seed>`` has been changed to ``--shuffle-sections=<section-glob>=<seed>``.
  Specify ``*`` as ``<section-glob>`` to get the previous behavior.

COFF Improvements
-----------------

* ...

MinGW Improvements
------------------

* ...

MachO Improvements
------------------

* Item 1.

WebAssembly Improvements
------------------------

