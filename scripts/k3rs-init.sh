CUR_DIR=$(pwd)
# aarch64
cargo zigbuild --release --target aarch64-unknown-linux-musl -p k3rs-init
# x86_64
# cargo zigbuild --release --target x86_64-unknown-linux-musl -p k3rs-init

rm -rf /tmp/initrd_new
mkdir -p /tmp/initrd_new/{dev,etc,mnt/rootfs,proc,run,sbin,sys,tmp}
cp ./target/aarch64-unknown-linux-musl/release/k3rs-init /tmp/initrd_new/sbin/k3rs-init
cd /tmp/initrd_new && find . | cpio -o -H newc 2>/dev/null | gzip > $CUR_DIR/build/kernel/initrd.img
echo "Done! initrd size: $(wc -c < $CUR_DIR/build/kernel/initrd.img) bytes"