# --server-only
rm -rf /tmp/k3rs-*
scripts/copy-vmlinux.sh
scripts/dev.sh --server-only