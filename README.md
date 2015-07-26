![CorTeX Framework](./public/img/logo.jpg) Framework
======

A general purpose processing framework for **Cor**-pora of **TeX** documents

<!-- [![Build Status](https://secure.travis-ci.org/dginev/rust-cortex.png?branch=master)](http://travis-ci.org/dginev/rust-cortex) -->

=======

** Warning: This repository is currently undergoing an active port of core functionality and is not ready for third-party use! **

=======

**Features**:
 * Lightning fast Rust implementation
 * Distributed processing and streaming data transfers via **ZeroMQ**
 * A flexible array of backends for Document, Annotation and Task provenance.
 * Open set of supported representations
 * Automatic dependency management of registered Services
 * Powerful workflow management and development support through the CorTeX web interface
 * Supports multi-corpora multi-service installations
 * Centralized storage, with distributed computing, motivated to enable collaborations across institutional and national borders.

**History**:
 * Rust reimplementation of the original Perl [CorTeX](https://github.com/dginev/cortex) stack.
 * Builds on the expertise developed during the [arXMLiv project](https://trac.kwarc.info/arXMLiv) at Jacobs University. 
 * In particular, CorTeX is a successor to the [build system](http://arxmliv.kwarc.info) originally developed by Heinrich Stamerjohanns.
 * The architecture tiered towards generic processing with conversion, analysis and aggregation services was motivated by the [LLaMaPUn](https://trac.kwarc.info/lamapun)
   project at Jacobs University.
 * The messaging conventions are motivated by work on standardizing [LaTeXML](http://dlmf.nist.gov/LaTeXML)'s log reports with Bruce Miller.

For more details, consult the [Installation](INSTALL.md) instructions and the [Manual](MANUAL.md).
