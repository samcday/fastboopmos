# fastboopmos

A set of [BootProfiles][] to enable live-booting official postmarketOS artifacts with [fastboop][].

Soon, you will be able to boot postmarketOS from the browser: https://www.fastboop.win/?channel=https://fastboopmos.samcday.com/edge.channel (Not yet, see [this issue][pmos-225])

For now, you must use the [fastboop CLI][quickstart]:

```sh
fastboop boot --serial https://fastboopmos.samcday.com/edge.channel
```

Wanna peek under the hood? [Cool.](./HACKING.md)

[fastboop]: https://github.com/samcday/fastboop
[quickstart]: http://docs.fastboop.win/user/#quickstart
[BootProfiles]: http://docs.fastboop.win/dev/BOOT_PROFILES/
[pmos-225]: https://gitlab.postmarketos.org/postmarketOS/postmarketos.org/-/work_items/225
