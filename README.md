![CorTeX Framework](./public/img/logo.jpg) Framework
======

**A general purpose processing framework for corpora of scientific documents**

[![Build Status](https://secure.travis-ci.org/dginev/CorTeX.png?branch=master)](http://travis-ci.org/dginev/CorTeX) [![API Documentation](https://img.shields.io/badge/docs-API-blue.svg)](http://dginev.github.io/CorTeX/cortex/index.html) [![License](https://img.shields.io/badge/license-MIT-blue.svg)](https://raw.githubusercontent.com/dginev/CorTeX/master/LICENSE) ![version](https://img.shields.io/badge/version-0.4.2-orange.svg) [![Dependabot Status](https://api.dependabot.com/badges/status?host=github&repo=dginev/CorTeX)](https://dependabot.com)


**News**:
  - **2019:** [![arXMLiv Homepage](https://img.shields.io/badge/web-corpora.mathweb.org-red.svg?style=flat&label=https&colorB=d33847)](https://corpora.mathweb.org) The CorTeX framework is recurringly converting >1.3 million articles from arXiv.org
  - **2018:** [![SIGMathLing resources](https://img.shields.io/badge/web-sigmathling.kwarc.info-blue.svg?style=flat&label=https&colorB=3895d3)](https://sigmathling.kwarc.info/resources/) First datasets produced by CorTeX are now available for reuse via the SIGMathLing interest group

**Features**:
 - [x] Safe and speedy Rust implementation
 - [x] Distributed processing and streaming data transfers via **ZeroMQ**
 - [x] Backend support for Document (via FileSystem) and Task (via PostgreSQL **≥9.5**) provenance.
 - [x] Representation-aware and -independent (TeX, HTML+RDFa, ePub, TEI, JATS, ...)
 - [x] Powerful workflow management and development support through the CorTeX web interface
 - [x] Supports multi-corpora multi-service installations
 - [x] Centralized storage, with distributed computing, motivated to enable collaborations across institutional and national borders.
 - [x] Routinely tested on 1 million scientific TeX papers from arXiv.org
 - [ ] Annotations backend and workflow (TODO)
 - [ ] Automatic dependency management of registered Services (TODO)

**History**:
 * Originally motivated by the desire to process any **Cor**-pus of **TeX** documents.
 * Rust reimplementation of the original Perl [CorTeX](https://github.com/dginev/deprecated-CorTeX) stack.
 * Builds on the expertise developed during the [arXMLiv project](https://kwarc.info/projects/arXMLiv/) at Jacobs University.
 * In particular, CorTeX is a successor to the [build system](https://link.springer.com/article/10.1007/s11786-010-0024-7) originally developed by Heinrich Stamerjohanns.
 * The messaging conventions are adopted from work on standardizing [LaTeXML](http://dlmf.nist.gov/LaTeXML)'s log reports with Bruce Miller.

For more details, consult the [Installation](INSTALL.md) instructions and the [Manual](MANUAL.md). The system is not ready for "off-the-shelf" third party use -- but there is a desire to prepare that, given community interest. Please feel invited to open an issue with a description of your target workflow!
