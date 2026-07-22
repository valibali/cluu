# /etc/rc.boot — system boot script (interpreted by procmgr at boot)
#
# Runs once at system boot. procmgr reads this file line-by-line and
# dispatches: start, wait, probe, mount.
# After rc.boot completes, procmgr proceeds to login.

# drivermgr auto-probes PCI + ACPI at its own startup (spawn_mode=spawn),
# spawning kbd/mouse/virtio-blk. Wait for it to finish probing.
wait drivermgr main
probe pci
probe acpi

# Core system services (parallel start — wait only where ordering matters)
start console
start vtmgr
start inputd
start compositor
