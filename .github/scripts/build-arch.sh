#!/usr/bin/env bash
# Run inside the disposable Arch build container.
set -euo pipefail

pacman-key --init
pacman -Syu --needed --noconfirm sudo
printf "%s\n" "OPTIONS+=('!debug')" > /etc/makepkg.conf.d/no-debug.conf
useradd -m builder
printf '%s\n' 'builder ALL=(root) NOPASSWD: /usr/bin/pacman' > /etc/sudoers.d/builder
chmod 0440 /etc/sudoers.d/builder

cp -R /input /home/builder/shisa
chown -R builder:builder /home/builder/shisa
cd /home/builder/shisa

as_builder() {
    runuser -u builder -- "$@"
}

as_builder makepkg --syncdeps --noconfirm --check
package=$(as_builder makepkg --packagelist)
[[ $package == */shisa-*-x86_64.pkg.tar.zst && -f $package ]]
pacman -Qip "$package"
pacman -Qlp "$package" | tee /out/package-files.txt
for path in \
    /usr/bin/{shisa,shisad,shisa-agent,shisa-settings} \
    /usr/lib/systemd/{system/shisad,user/shisa-agent}.service \
    /usr/share/polkit-1/{actions/io.github.RiverOnVenus.Shisa.policy,rules.d/49-shisa-admin.rules}; do
    grep -Fx "shisa $path" /out/package-files.txt
done
pacman -S --needed --noconfirm namcap
namcap PKGBUILD "$package" || echo "::warning::namcap reported a diagnostic"
cp -- "$package" /out/
