# oguRPChik 🥒

<div align="center">
  <img src="/Unusual-Bananas-0.png" width="300" alt="Ogurpchik Logo">
  <br>

[![Crates.io](https://img.shields.io/crates/v/ogurpchik.svg)](https://crates.io/crates/ogurpchik)
[![Docs.rs](https://docs.rs/ogurpchik/badge.svg)](https://docs.rs/ogurpchik)
[![License](https://img.shields.io/crates/l/ogurpchik.svg)](LICENSE)
</div>

> **A transport-agnostic RPC framework for stream and memory-based communication. Built with high-performance primitives to deliver medium-performance results.**

## 🧐 Motivation

This crate is actively used in my main project for duplex communication (Host <-> VM, Host <-> Plugins).

However, let's be honest: I didn't extract it into a separate library for "better modularity" or "architectural purity". I did it because the pun **Ogurpchik** (*Ogurets* + *RPC*) popped into my head, and I simply needed a public repository to make the joke official.

## 🚀 Features

- **Transport Agnostic**: Works over TCP, VSOCK, SHM, or any other communication backend you choose to implement. 
- **Message Flexible**: Supports both data-owning (Serde) and zero-copy view formats (rkyv).

## License

MIT