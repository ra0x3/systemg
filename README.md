# systemg

![CI](https://github.com/ra0x3/systemg/actions/workflows/ci.yaml/badge.svg)

# ⚡ Systemg - A Lightweight Process Manager

Systemg is a **simple, fast, and dependency-free process manager** written in Rust.  
It aims to provide **a minimal alternative to systemd** and other heavyweight service managers, focusing on **ease of use**, **clarity**, and **performance**.

## 🚀 Why Systemg?

Traditional process managers like **systemd** are complex, heavy, and introduce unnecessary dependencies.  
Systemg offers a **lightweight**, **configuration-driven** solution that’s **easy to set up and maintain**.

## ✨ Features

- **Declarative YAML Configuration** - Define services, dependencies, and restart policies easily.
- **Automatic Process Monitoring** - Restart crashed services based on custom policies.
- **Environment Variable Support** - Load variables from `.env` files and per-service configurations.
- **Minimal & Fast** - Built with Rust, designed for performance and low resource usage.
- **No Root Required** - Unlike systemd, it doesn’t take over PID 1.

---

## 🔄 Comparison vs Alternatives

| Feature            | Systemg 🚀       | systemd 🏢         | Supervisor 🛠️   | Docker Compose 🐳  |
|--------------------|-----------------|-----------------|-----------------|------------------|
| **Lightweight**    | ✅ Yes           | ❌ No (Heavy)   | ❌ No (Python)  | ❌ No (Containers) |
| **No Dependencies**| ✅ Yes           | ❌ No (DBus, etc.) | ❌ No (Python)  | ❌ No (Docker)    |
| **Simple Config**  | ✅ YAML          | ❌ Complex Units | ✅ INI          | ✅ YAML          |
| **Process Monitoring** | ✅ Yes      | ✅ Yes         | ✅ Yes         | ✅ Yes          |
| **PID 1 Required?**| ❌ No            | ✅ Yes         | ❌ No          | ❌ No           |
| **Handles Dependencies?** | ✅ Yes  | ✅ Yes         | ❌ No          | ✅ Yes          |

---

## 📖 Getting Started

### **1️⃣ Install Systemg**
```sh
cargo install systemg
