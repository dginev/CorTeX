:exclamation: **Warning**: This repository is undergoing first stability runs and is not ready for third-party use! We have converted ~1 million articles from arXiv.org with this implementation, and consider the CorTeX job manager largely stable. Next, we are investigating fragility of some peripheral workers in the coming weeks.

![CorTeX Framework](./public/img/logo.jpg) Framework
======

**A general purpose processing framework for corpora of scientific documents**

[![Build Status](https://secure.travis-ci.org/dginev/CorTeX.png?branch=master)](http://travis-ci.org/dginev/CorTeX) [![Coverage Status](https://coveralls.io/repos/dginev/CorTeX/badge.svg?branch=master&service=github)](https://coveralls.io/github/dginev/CorTeX?branch=master) [![API Documentation](https://img.shields.io/badge/docs-API-blue.svg)](http://dginev.github.io/CorTeX/cortex/index.html) [![license](http://img.shields.io/badge/license-MIT-blue.svg)](https://raw.githubusercontent.com/dginev/CorTeX/master/LICENSE)

**Features**:
 - [x] Safe and speedy Rust implementation
 - [x] Distributed processing and streaming data transfers via **ZeroMQ**
 - [x] Backend support for Document (via FileSystem), Annotation (via ?) and Task (via Postgresql) provenance.
 - [x] Representation-aware and -independent (TeX, HTML+RDFa, ePub, TEI, JATS, ...)
 - [ ] Automatic dependency management of registered Services (TODO)
 - [x] Powerful workflow management and development support through the CorTeX web interface
 - [x] Supports multi-corpora multi-service installations
 - [x] Centralized storage, with distributed computing, motivated to enable collaborations across institutional and national borders.
 - [x] Routinely tested on 1 million scientific TeX papers from arXiv.org

**History**:
 * Originally motivated by the desire to process any **Cor**-pus of **TeX** documents.
 * Rust reimplementation of the original Perl [CorTeX](https://github.com/dginev/deprecated-CorTeX) stack.
 * Builds on the expertise developed during the [arXMLiv project](https://trac.kwarc.info/arXMLiv) at Jacobs University. 
 * In particular, CorTeX is a successor to the [build system](http://arxmliv.kwarc.info) originally developed by Heinrich Stamerjohanns.
 * The architecture tiered towards generic processing with conversion, analysis and aggregation services was motivated by the [LLaMaPUn](https://trac.kwarc.info/lamapun)
   project at Jacobs University.
 * The messaging conventions are motivated by work on standardizing [LaTeXML](http://dlmf.nist.gov/LaTeXML)'s log reports with Bruce Miller.

For more details, consult the [Installation](INSTALL.md) instructions and the [Manual](MANUAL.md).
