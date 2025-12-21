# **The "Mac-Like" Asahi Workstation: Design Manifesto**

### **1\. The Core Philosophy: "Adapt the Machine, Not the Human"**

We reject the notion that migrating to Linux requires abandoning a decade of muscle memory. The operating system is a tool, and it should bend to the user's existing neural pathways.

- **The Prime Directive:** **Physical keys must retain their semantic meaning.**
  - **Command (⌘)** is for _Application & System Control_ (New Tab, Close, Switch App).
  - **Control (⌃)** is for _Terminal & Context_ (Interrupt Signal, VIM chords).
  - **Option (⌥)** is for _Alternative Actions_ (Special chars, window snapping, "God Mode" system overrides).
- **The Implementation:** We do not rely on fragile GUI reconfiguration tools. We solve this at the **Kernel Input Level** (keyd). We use **Layer Inheritance** (:A or :M) to ensure that modifier keys maintain their "held" state for complex interactions like the Application Switcher, rather than firing simple macros.

### **2\. The Connectivity Doctrine: "Pragmatism over Purity"**

We acknowledge that we are running reverse-engineered drivers on undocumented Apple Silicon hardware. The standard Linux networking stack (wpa_supplicant) is too old and rigid for this fragile environment.

- **The Conflict:** Apple's Broadcom firmware is a "Black Box" that crashes (Error \-52) when faced with aggressive scanning or unexpected packet timing.
- **The Strategy:** **The "Golden Stack."**
  - We reject the default. We adopt **iwd** (Intel Wireless Daemon) because its modern architecture offloads cryptography to the kernel and scans passively, respecting the firmware's fragility.
  - We disable "Smart" features. If Roaming or Power Save causes the firmware to panic, we ruthlessly disable them (roamoff=1). Connectivity \> Features.

### **3\. The Visual Paradigm: "Virtualizing the Glass"**

We acknowledge that modern displays (Ultrawide) and Input devices (Glass Trackpads) behave differently than the standard PC hardware Linux was built for.

- **The Ultrawide Problem:** "Maximize" is a legacy concept on a 49" screen.
- **The Solution:** **Virtualize the Monitor.** We do not let windows fill the physical screen. We use **Tiling Shell** to define "Virtual Monitors" (Layouts). We treat the screen as a canvas of defined slots, not a bucket to be filled.
- **The Input Feel:** A trackpad is not a mouse. We disable "Physical Click" logic (which is jittery on Linux drivers) and enforce **Tap-to-Drag with Lock**. This mimics the fluid, low-pressure interaction model of macOS.

### **4\. The Compatibility Layer: "Encapsulate the Legacy"**

We accept the **16k Page Size** reality of Apple Silicon. We do not try to force square pegs (4k x86 apps) into round holes.

- **The Strategy:** **Micro-Virtualization (muvm).**
  - We do not pollute the host OS with x86 libraries that will crash.
  - We treat x86 applications (Steam) and Android environments (Waydroid) as "foreign matter." We encapsulate them in a lightweight, transparent 4k-page MicroVM.
  - _Principle:_ If it requires 4k pages, it goes in the box. We do not fight the kernel.

### **5\. The Hardware Workaround: "Lateral Thinking"**

When the direct path is blocked by missing drivers (e.g., Thunderbolt/USB4 Display Output), we do not wait for upstream fixes. We find the **Lateral Path**.

- **Case Study: The Dock.**
  - _Blocked Path:_ Thunderbolt PCIe tunneling is broken/WIP.
  - _Lateral Path:_ Force "Dumb USB" mode. Use a standard USB-C cable to bypass the Thunderbolt negotiation, forcing the hardware to fall back to standard USB protocols which _are_ supported.
  - _Blocked Path:_ Dual Monitor support on M-Series chips.
  - _Lateral Path:_ **DisplayLink**. Bypass the GPU entirely and render pixels via CPU/USB.

### **6\. The Portability Doctrine: "Truthful by Default"**

This project lives across machines (Asahi vs non‑Asahi), distros, and desktop environments. Portability does not mean “works everywhere”; it means **fails honestly** and **does not hallucinate success**.

- **The Rule:** If a capability is unavailable (missing tool, missing permission, non‑systemd system), we record it as **skipped** with the reason.
- **The Anti‑Goal:** Never produce “successfully empty” output that looks healthy but is really “no access.”
- **The Consequence:** Every check/probe has an explicit **capability boundary**: what it needs (tooling/privileges/platform) and what it does when that boundary is not met.

### **7\. The Empiricism Doctrine: "Evidence + Deletion"**

Workstation tweaks rot when they are justified by vibes. This project treats changes as experiments.

- **Every change has evidence.** Capture a before/after snapshot, compare, and keep the artifact.
- **Every change has rollback.** If we cannot delete it cleanly, we do not trust it.
- **Every probe has deletion criteria.** A probe exists to retire folklore and manual rituals; when it no longer deletes confusion, it should be removed.

### **Decision Matrix for Future Issues**

When a new problem arises, apply this logic:

1. **Is it a Muscle Memory conflict?** \-\> Solve it in keyd (Kernel level), not in the App.
2. **Is it a Hardware Driver crash?** \-\> Simplify the stack. Disable "Smart" features (Roaming, Power Save, Aggressive Scanning).
3. **Is it a Software Incompatibility (Page Size)?** \-\> Don't patch it. Run it in muvm.
4. **Is the Hardware feature missing (Thunderbolt)?** \-\> Find the "Dumb" standard alternative (USB 3.0, DisplayLink).
5. **Is this a portability boundary (missing tool/permission/platform)?** \-\> Mark it as skipped with the reason; add a capability gate.
6. **Is this a tweak without evidence?** \-\> Add a snapshot + diff + rollback loop before trusting it.
