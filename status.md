# Status Implementasi Migo

Dokumen ini adalah turunan yang mudah dibaca dari **migo.md section 177 (IMPLEMENTATION
STATUS)**. migo.md tetap satu-satunya sumber kebenaran; kalau keduanya berbeda, migo.md yang
benar dan file ini yang salah. Gate `python3 tools/scripts/brief-audit.py` (41 pemeriksaan)
menegakkan section 177 secara mekanis: ia menolak crate yang ditandai BUILT tanpa test, crate
yang punya test tapi masih ditandai belum, dan crate yang muncul di dua blok sekaligus. Di
sebelahnya ada `infra-audit.py` (12 pemeriksaan), yang membaca berkas penyebaran di `infra/`
tanpa menyalakan satu pun container, dan `pydeps-audit.py` (6 pemeriksaan), yang memastikan
baris `pip install` pada job CI memasang tepat modul pihak ketiga yang benar-benar diimpor
`tools/`. Ketiganya dijalankan job `gates` pada `.github/workflows/ci.yml` lewat
`make brief-check`, `make infra-check`, dan `make pydeps-check`, bersama empat gate pembaca
berkas lain: `protocol-check`, `entity-check`, `vector-check`, dan `kotlin-check`.

Job `audit` melaporkan advisory tanpa memblokir merge dan membawa tepat satu pengecualian
beralasan di `server/.cargo/audit.toml`, yaitu RUSTSEC-2026-0235 pada `rkyv`. `rkyv` masuk
sebagai feature opsional `rust_decimal` yang tidak dinyalakan siapa pun, jadi ia tercatat di
`Cargo.lock` tanpa pernah dikompilasi: satu build menghasilkan 40 artefak `rust_decimal` dan nol
artefak `rkyv`. Pengecualian itu wajib dicek ulang pada setiap kenaikan sea-orm dan dihapus
begitu resolusinya pindah ke 0.8.17 atau lebih baru, karena ignore yang hidup lebih lama
daripada alasannya adalah cara sebuah advisory sungguhan diloloskan diam-diam.

Terakhir diselaraskan: 27 Agustus 2026. Penyelarasan ini memverifikasi ulang setiap angka di
bawah terhadap pohon yang bersih, yaitu ketujuh gate, 23 anggota workspace, dan 1598 test Rust
bersama 10 doc-test dan 251 test TypeScript yang semuanya hijau, lalu menutup empat hal dari
status sebelumnya. Yang pertama: empat invariant migo-gateway yang sebelumnya hanya ditulis
di kepala suite (backpressure, parse-before-alloc, push-only, hygiene) kini diuji dengan
lima test baru di `migo-gateway/tests/gateway.rs`. Yang kedua: opcode 144
`NOTIFICATION_EVENT`, satu-satunya opcode IDL yang masih SCHEMA, sekarang memiliki jalur
server-side `Gateway::emit_notification` yang round-trip diuji dengan satu test integrasi
di `migod/tests/migod.rs` yang menggerakkan gateway sungguhan melalui FakeTransport. Yang
ketiga: smoke test dua-node end-to-end di `tools/2node/run.sh` yang menjalankan dua
instance migod berdiri sendiri (port berbeda, database postgres berbeda, node identity
berbeda) lalu menggerakkan chat bot TypeScript di `tools/chatbot/` yang mendaftarkan
dua akun, membuka percakapan langsung, dan mengirim 10 round-trip pesan — semua dari
nol sampai pesan tervalidasi dalam satu skrip. Yang keempat: release `v0.1.0` yang
sebelumnya dibuat sebagai `draft: true` (sehingga semua binary yang di-upload
tidak terlihat di halaman publik) sudah dipublish manual, dan `release.yml` dipatch
agar `gh release create` memakai `--draft=false` dan me-publish ulang kalau
sebuah run menemukan kembali release yang masih draft. Commit sebelumnya menutup
cacat terbuka 8b dengan mengimplementasikan `Dispatcher::authorize_topics` di
`migo-gateway` dan `AppDispatcher` di `migod`, lengkap dengan 3 test di
`migo-gateway/tests/gateway.rs` dan 5 test di `migod/tests/migod.rs` yang menutup
keempat perilaku yang dituntut kepala suite gateway. Commit sebelumnya memperbaiki
job `gates` setelah gate konformans pecah di CI, menambahkan gate ketujuh `make pydeps-check`
supaya kelas kegagalan itu tidak terulang, dan memberi job advisory satu pengecualian
beralasan. Sebelum itu enam crate terakhir (`migo-games`, `migo-bots`,
`migo-federation`, `migo-gateway`, `migo-api`, dan `migod`) berpindah bersama `packages/sdk`,
`clients/web`, dan `tools/loadgen` ke BUILT.

## Ringkasan

| Kategori                                                       | Jumlah                            |
| -------------------------------------------------------------- | --------------------------------- |
| Selesai: kode, test, clippy bersih                             | 23 crate Cargo + 12 komponen lain |
| Kode lengkap, test belum ditulis (workspace Cargo)             | 0, blok ini kosong                |
| Kode lengkap, test belum ditulis (di luar workspace Cargo)     | 2 komponen                        |
| Kode lengkap, kompilasi diverifikasi di CI, test belum ditulis | 1 komponen                        |
| Belum ada kode sama sekali                                     | 1 komponen                        |
| Sudah di schema dan codegen, handler belum ditulis             | 3 item                            |
| Baru ada di dokumen                                            | 16 item                           |
| Test yang hijau pada commit ini                                | 1598 Rust + 10 doc-test + 251 TS  |
| Cacat terbuka yang belum diperbaiki                            | 0, lihat bagian 8                 |

Tidak ada satu pun test yang gagal, dan tidak ada satu pun `#[ignore]` di seluruh workspace.
Yang dilewati rustdoc hanyalah enam contoh dokumentasi bertanda ` ```ignore ` pada `migo-bots`,
`migo-economy`, `migo-games`, `migo-moderation`, `migo-notify`, dan `migo-social`: keenamnya
adalah cuplikan ilustrasi yang menuntut graph aplikasi yang sudah hidup, jadi perilakunya
dipakukan oleh test integrasi crate-nya masing-masing dan bukan oleh doc-test.

## 1. Selesai: kode lengkap, ada test, clippy bersih

Sebuah item hanya boleh berada di sini bila `cargo build`, `cargo clippy --all-targets` tanpa
satu pun peringatan, `cargo doc` tanpa intra-doc link rusak, dan `cargo test` semuanya hijau.

| Komponen                   | Isi singkat                                                                                                                          | Test       |
| -------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ | ---------- |
| `migo-core`                | id, timestamp, error, config, metrics, random, secret, clock                                                                         | 66         |
| `migo-wire`                | codec frame: varint, zigzag, MSE, flag, limit                                                                                        | 91         |
| `migo-protocol`            | hasil codegen IDL: opcode, error code, feature bit, fault                                                                            | 27         |
| `migo-crypto`              | Ed25519, X25519, X3DH, double ratchet, sender key, AEAD, KDF, MAC                                                                    | 129        |
| `migo-store`               | 10 trait domain, backend SeaORM dan backend in-memory                                                                                | 96         |
| `migo-cache`               | 6 trait cache, backend in-memory dan Redis dengan Lua atomik                                                                         | 116        |
| `migo-ratelimit`           | token bucket berbasis cost di atas 7 surface section 120                                                                             | 33         |
| `migo-auth`                | registrasi, sign in, access token 130 byte, rotasi refresh                                                                           | 66         |
| `migo-messaging`           | kirim, edit, hapus, reaksi, receipt, riwayat, envelope E2E                                                                           | 38         |
| `migo-presence`            | presence per device di cache, TTL tiga kali heartbeat                                                                                | 26         |
| `migo-economy`             | listing, wallet, statement, purchase, transfer, mata uang in-app                                                                     | 12         |
| `migo-keys`                | publish dan bundles: identity key, signed prekey, one-time prekey                                                                    | 34         |
| `migo-rooms`               | 15 metode Roomkeeper: pembuatan, join, roster, peran, moderasi                                                                       | 108        |
| `migo-social`              | 19 metode Graph: pertemanan, follow, block, favourite, privasi                                                                       | 111        |
| `migo-media`               | 8 metode Library: begin, status, commit, abort, fetch_url, delete                                                                    | 50         |
| `migo-moderation`          | 7 metode Warden: laporan, queue, keputusan, aksi, audit, skor                                                                        | 84         |
| `migo-notify`              | 8 metode Notifier: notify, inbox, badge, token push, sweep                                                                           | 63         |
| `migo-games`               | 6 metode Referee: katalog, mulai, main, selesai, papan skor                                                                          | 95         |
| `migo-bots`                | 7 metode Bots: register, authenticate, rotate_token, izin                                                                            | 96         |
| `migo-federation`          | 17 metode Mesh: handshake, peer, urutan link, antrean keluar                                                                         | 71         |
| `migo-gateway`             | transport realtime: mesin state koneksi, frame, heartbeat, otorisasi SUBSCRIBE, backpressure, parse-before-alloc, push-only, hygiene | 21         |
| `migo-api`                 | permukaan REST/JSON layer 4 yang diizinkan section 118                                                                               | 65         |
| `migod`                    | composition root layer 5, argumen, penolakan startup, graph, AppDispatcher::authorize_topics, Gateway::emit_notification round-trip  | 69         |
| `packages/protocol`        | paket TypeScript hasil generate dari IDL yang sama                                                                                   | 11         |
| `packages/wire`            | codec frame TypeScript, pasangan dari `migo-wire`                                                                                    | 16         |
| `packages/crypto`          | primitif kripto web di atas paket `@noble`                                                                                           | 21         |
| `packages/sdk`             | SDK TypeScript di atas wire, protocol, dan crypto                                                                                    | 56         |
| `clients/web`              | PWA Next.js full client side, dilayani di port 19991                                                                                 | 63         |
| `tools/protocol-codegen`   | generator Rust dan TypeScript dari IDL                                                                                               | dipakai CI |
| `tools/entity-codegen`     | generator entity SeaORM dari schema                                                                                                  | dipakai CI |
| `tools/loadgen`            | pembangkit beban yang menggerakkan MigoClient sungguhan                                                                              | 84         |
| `tools/chatbot`            | dua akun, satu percakapan langsung, sepuluh pesan bolak-balik lewat gateway; hanya end-to-end smoke                                  | 0          |
| `tools/2node`              | dua migod berdiri sendiri (port + database + node identity berbeda) plus skrip run.sh end-to-end                                     | 0          |
| `shared/protocol/schema`   | IDL itu sendiri: 29 opcode, error code, feature bit                                                                                  | gate       |
| `shared/protocol/vectors`  | vector konformans wire dan kripto                                                                                                    | 2 runner   |
| `tools/vectors`            | pembangkit dan pemverifikasi vector                                                                                                  | dipakai CI |
| `.github/workflows/ci.yml` | seluruh build, lint, test, dan rilis binary                                                                                          | jalan      |

Tidak ada baris di tabel itu yang membawa penanda pada commit ini. Baris `migo-gateway`
sebelumnya ditandai `(8b)` karena invariant yang ditulis di kepala suite-nya tidak punya
test dan ternyata juga tidak punya kode; penanda itu sudah dicabut setelah cacat di baliknya
diperbaiki dan diberi test pada commit yang sama. Cerita dan daftar test ada di bagian 8.

Angka pada kolom Test adalah jumlah test case yang benar-benar dijalankan `cargo test` dan
`pnpm -r test` pada commit ini, bukan jumlah atribut `#[test]` di disk. Keduanya tidak selalu
sama, karena satu atribut dapat menjalankan banyak case: contract suite `migo-store` dan
`migo-cache` misalnya dijalankan terhadap dua backend sekaligus. Sepuluh doc-test dihitung
terpisah, masing-masing satu pada `migo-api`, `migo-auth`, `migo-core`, `migo-gateway`,
`migo-messaging`, `migo-presence`, `migo-protocol`, `migo-ratelimit`, `migo-rooms`, dan
`migo-wire`.

## 2. Kode lengkap, test belum ditulis (workspace Cargo)

Kosong pada commit ini. Setiap anggota `server/Cargo.toml` sudah punya test yang dijalankan CI.
Enam crate terakhir yang pindah dari sini ke bagian 1 adalah `migo-games`, `migo-bots`,
`migo-federation`, `migo-gateway`, `migo-api`, dan `migod`. Blok ini sengaja tidak dihapus dari
migo.md, karena ia adalah tempat yang benar bagi crate berikutnya yang kodenya selesai lebih
dulu daripada test-nya.

Satu catatan kejujuran yang juga tertulis di migo.md: kepala suite `migo-gateway` menyebut
delapan invariant, dan 13 test yang ada baru menutup dua di antaranya, yaitu urutan frame yang
ditegakkan dan penutupan atas kemauan server. Enam yang lain, yaitu backpressure yang terbatas
dan gagal menutup, pemeriksaan ukuran sebelum parse, otorisasi yang dibaca dan bukan dipercaya
dari frame, wire yang push-only, higiene log dan metrik, serta limit yang berlaku tepat di
batasnya, masih menunggu test. Itulah pekerjaan berikutnya di crate itu.

Satu dari enam itu ternyata bukan sekadar belum dites. Membaca crate itu untuk menyiapkan
test-nya menunjukkan bahwa otorisasi topic tidak hanya belum dites melainkan belum ada: itu
cacat terbuka pertama pada dokumen ini dan dicatat di bagian 8. Pelajarannya persis alasan aturan
di bagian 9 nomor 2 ada, yaitu bahwa sebuah invariant yang ditulis di kepala suite tetapi tidak
punya test bukan invariant, melainkan niat.

## 3. Kode lengkap di luar workspace Cargo, test belum ditulis

| Komponen          | Isi singkat                                            | Keadaan                                         |
| ----------------- | ------------------------------------------------------ | ----------------------------------------------- |
| `clients/desktop` | native desktop client Rust di atas eframe dan egui     | clippy bersih, belum ada satu pun test          |
| `infra`           | Dockerfile, compose, Kubernetes, Terraform, dan README | gate statis `make infra-check`, belum ada smoke |

Keduanya sengaja tidak ditandai selesai. `clients/desktop` lulus `cargo clippy --all-targets`
tanpa peringatan tetapi belum punya test sama sekali. `infra` sejak commit ini punya gate statis
yang memeriksa 12 hal yang dapat dibaca dari berkasnya (image yang dipin ke tag tetap, tidak ada
material kunci atau nilai berbentuk secret di luar dua konstanta development yang di-allow-list,
tidak ada container privileged, host namespace, atau mount host yang dapat ditulis, requests,
limits, dan kedua probe pada setiap workload Kubernetes, tidak ada dua service yang menerbitkan
host port yang sama, dan web yang menerbitkan tepat port 19991), tetapi gate itu tidak
menyalakan satu pun container sehingga bukan pengganti smoke test.

## 4. Kode lengkap, kompilasi diverifikasi di CI, test belum ditulis

| Komponen          | Isi singkat                                                                        |
| ----------------- | ---------------------------------------------------------------------------------- |
| `clients/android` | SDK dan app Android Kotlin, dikompilasi hanya oleh `.github/workflows/android.yml` |

Kotlin tidak dapat dikompilasi di mesin kerja ini karena tidak ada JDK dan tidak ada cara
memasangnya, jadi satu-satunya bukti kompilasi datang dari CI. Setiap berkas Kotlin baru
diverifikasi lokal dengan `python3 tools/scripts/kotlin-lint.py`.

## 5. Belum ada kode sama sekali

| Komponen    | Isi singkat                                                                   |
| ----------- | ----------------------------------------------------------------------------- |
| `tests/e2e` | uji ujung ke ujung yang menjalankan server sungguhan bersama client sungguhan |

Keduanya menuntut server hidup bersama PostgreSQL, jadi tempatnya adalah job CI dan bukan mesin
kerja ini. Ada satu direktori kosong kedua di pohon, yaitu `tests/load`, yang tidak disebut
section 177 dan tidak dilacak Git karena Git tidak melacak direktori kosong; perannya sudah
dipegang `tools/loadgen`, yang ada di bagian 1.

## 6. Sudah di schema dan codegen, handler belum ditulis

| Item                                     | Keterangan                                                       |
| ---------------------------------------- | ---------------------------------------------------------------- |
| Opcode `NOTIFICATION_EVENT` bernomor 144 | satu-satunya dari 29 opcode IDL yang belum pernah menyentuh wire |
| Struct notification                      | satu-satunya kelompok struct yang di-codegen tanpa pemakai       |
| Feature bit 0 sampai 15                  | sudah dinegosiasikan handshake, belum semuanya mengubah perilaku |

## 7. Baru ada di dokumen

Enam belas item berikut sengaja masih spesifikasi. Semuanya sudah tertulis lengkap di migo.md
tetapi belum satu pun byte kodenya ditulis:

- Metadata block pada section 141 dan flag bit `0x40`
- Opcode messaging tambahan 40 sampai 42
- Opcode social 113 sampai 117
- Opcode media 128 sampai 133
- Opcode notification 145 dan 146
- Opcode economy 160 sampai 162
- Opcode bot 178 sampai 180
- Opcode moderation 192 sampai 194
- Opcode federation 208 sampai 221
- Opcode call 224 sampai 238
- Feature bit 16 sampai 20
- Call signaling dan media architecture pada section 165 dan 166
- Voice note protocol pada section 167
- Requirement produk voice note section 179 dan requirement produk call section 180
- Media architecture pada section 168
- Federation protocol pada section 169 dan 170

## 8. Cacat produk yang ditemukan oleh tahap test

Tahap test bukan pekerjaan tulis ulang. Sejauh ini ia menemukan lima belas cacat nyata pada
kode yang sudah dianggap selesai, dan semuanya diperbaiki pada commit yang sama dengan test yang
menemukannya. Baris keenam belas datang bukan dari test melainkan dari pembacaan crate
`migo-gateway` untuk menyiapkan test atas invariant otorisasi yang sudah ditulis di kepala
suite-nya, dan tetap dicatat di sini karena ia adalah cacat pada sesuatu yang sudah dianggap
selesai: otorisasi SUBSCRIBE tidak ada, bukan rusak. Baris ketujuh belas datang bukan dari
test melainkan dari pipeline-nya sendiri. Yang kedelapan belas — cacat terbuka 8b — sudah
diperbaiki pada commit ini, dan itulah commit yang menandai `migo-gateway` layak tanpa
penanda.

### 8a. Sudah diperbaiki pada commit yang sama dengan penemuannya

| Crate             | Cacat                                                                                                                                                                                                                                                                                     | Perbaikan                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| ----------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `migo-social`     | `pending` melaporkan permintaan yang belum dijawab sebagai sudah disetujui                                                                                                                                                                                                                | membaca kolom keadaan yang benar                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| `migo-social`     | `block` menghapus edge tanpa menghitungnya, sehingga hitungan relasi melenceng                                                                                                                                                                                                            | penghapusan ikut mengurangi hitungan                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| `migo-media`      | tidak ada pemeriksaan identitas sama sekali di seluruh crate                                                                                                                                                                                                                              | `require_identity` sebelum pemungutan biaya di 7 metode                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| `migo-media`      | lebar, tinggi, dan durasi diperiksa di `begin` lalu dibuang sebelum ditulis                                                                                                                                                                                                               | format tiket naik ke versi dua dan membawa ketiganya                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| `migo-media`      | `commit` yang diulang ditolak sebagai objek yang sudah ada                                                                                                                                                                                                                                | dijawab dari baris yang ada tanpa menyentuh penghitung                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| `migo-moderation` | `file_report` menerima caller yang membawa akun tanpa device                                                                                                                                                                                                                              | identitas akun dan device diperiksa sebelum biaya dipungut                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| `migo-store`      | `open_reports` in-memory mengurut menurut urutan tulis, PostgreSQL menurut `created_at`                                                                                                                                                                                                   | double diurutkan menurut `created_at` lalu `report_id`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| `migo-notify`     | lima metode yang menghadap client tidak memeriksa identitas pemanggil                                                                                                                                                                                                                     | `require_identity` sebelum pemungutan biaya                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| `migo-cache`      | `CacheKey::new` menolak underscore, sehingga scope coalescing panic di build debug                                                                                                                                                                                                        | assertion menerima underscore, titik dua tetap dilarang                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| `migo-store`      | token CAS game adalah timestamp milidetik, jadi dua langkah dalam milidetik yang sama membuatnya tidak bergerak dan langkah kedua menimpa langkah pertama tanpa pernah melihatnya                                                                                                         | token didorong melewati nilai yang baru saja dicocokkan pada kedua backend, dan contract case memakukannya                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| `migo-bots`       | pemanggil tanpa identitas dimeter terhadap akun yang disebut permintaannya, sehingga penyerang dapat menguras budget akun orang lain tanpa membayar apa pun                                                                                                                               | identitas diperiksa dan ditolak sebelum limiter disentuh                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| `migo-core`       | staging dan production menerima kredensial database `migo:migo` yang terdokumentasi terbuka di compose dan CI                                                                                                                                                                             | startup ditolak dengan menyebut field-nya tanpa menggemakan kredensialnya                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| `clients/web`     | ketika server menahan pesan manusia, SDK melipat pesan kosong menjadi symbol mesin dan UI menampilkannya, sehingga NOT_FOUND dan PRIVACY_RESTRICTED yang sengaja dibuat identik menjadi dapat dibedakan                                                                                   | pesan server hanya ditampilkan bila benar-benar ada, selebihnya satu baris generik                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| `tools/loadgen`   | logger menulis barisnya tanpa redaksi dan laporan menggemakan URL server yang utuh beserta userinfo-nya                                                                                                                                                                                   | setiap baris logger lewat `redact`, dan laporan melewatkan kedua URL lewat `sanitizeUrl`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| `ci.yml`          | menyematkan interpreter Python untuk satu gate ikut menyembunyikan modul yang kebetulan sudah ada di image runner, sehingga generator vector kripto kehilangan `cryptography` dan gate konformans pecah di CI padahal hijau di lokal                                                      | kedua modul dipasang eksplisit dalam satu langkah, dan `make pydeps-check` membandingkan daftar itu dengan impor `tools/` yang sebenarnya di kedua arah                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| `migo-gateway`    | `handle_subscribe` menitipkan otorisasi pada isi frame: siapa pun yang lolos handshake bisa menyebut topik apa pun dan mulai menerima fan-out percakapan, room, dan presence orang lain, dan satu frame yang dibayar satu tagihan rate limiter dapat meminta puluhan ribu topik sekaligus | satu metode batch `authorize_topics` di trait `Dispatcher` dengan default menolak segalanya, `TopicRequest` yang tidak bisa membalas atau mempublikasikan sehingga tidak dapat dipakai sebagai `ClientContext` secara tak sengaja, pemotongan pada `MAX_SUBSCRIPTIONS` sebelum domain ditanya, daftar `rejected` yang sama untuk tiga alasan dan tanpa alasan agar bukan-anggota tidak dapat dibedakan dari tidak-ada, dan `AppDispatcher` di `migod` yang memetakan Conversation ke `Messaging::is_participant`, Room ke `Roomkeeper::authorize` dengan mask kosong, User ke akun pemanggil sendiri atau `Social::may_interact` dengan `Interaction::LastSeen`, dan Unknown serta Game ke penolakan; tiga test gateway dan lima test migod di-commit yang sama menutup keempat perilaku yang dituntut kepala suite                                                                                         |
| `migo-gateway`    | kepala suite gateway menyatakan empat invariant tambahan (backpressure, parse-before-alloc, push-only, hygiene) yang tidak punya test pada commit sebelumnya                                                                                                                              | `backpressure_drops_droppable_and_coalescable_but_never_critical` menutupi kelas tiga delivery (Critical tidak pernah di-drop, Droppable dan Coalescable di-drop dan dihitung) dengan kapasitas antrian satu slot, `an_oversize_frame_is_refused_before_any_allocation` menutupi bahwa `FRAME_TOO_LARGE` muncul sebelum `Frame::decode` menyentuh header atau payload, `the_wire_is_push_only_and_has_no_request_or_response_opcode` dan `a_server_to_client_opcode_from_a_client_closes_the_session` menutupi bahwa opcode server-to-client tidak pernah disalahartikan sebagai request dan gateway menolak mereka sebagai protocol violation, `error_frames_carry_only_their_public_face` menutupi bahwa empat jalur kesalahan (non-HELLO opener, unparseable opener, bad inline token) tidak pernah membocorkan penanda internal ke klien dan metrik registry tidak pernah memuat id akun atau perangkat |

### 8b. Sudah ditutup pada commit ini

Subbagian ini pada commit sebelumnya berjudul "Ditemukan dan belum diperbaiki" dan memegang
satu cacat terbuka, yaitu `migo-gateway` yang menitipkan otorisasi `SUBSCRIBE` pada isi frame.
Commit ini menutupnya. Cerita singkat tentang apa yang dulu ada di sini layak disimpan
sebab ia menjelaskan mengapa aturan di bagian 9 nomor 2 tidak cukup tanpa test yang berani
bertanya.

`handle_subscribe` di `server/crates/migo-gateway/src/connection.rs` menagih rate limiter,
membaca `SubscribeRequest`, lalu memanggil `hub.subscribe` dengan daftar topic dari frame
apa adanya. `Hub::subscribe` di `src/hub.rs` hanya membandingkan jumlah langganan yang
dipegang sesi terhadap `MAX_SUBSCRIPTIONS`. Trait `Dispatcher` di `src/dispatch.rs` punya
tepat satu metode, yaitu `dispatch`, dan tidak punya kait otorisasi, sehingga tidak ada satu
pun crate domain yang pernah ditanya apakah pemanggil berhak atas sebuah topic. Akibatnya,
sesi mana pun yang lolos handshake dapat menyebut `Topic` apa saja dan mulai menerima
fan-out-nya:

- `TopicKind::Conversation` memberi seluruh metadata percakapan orang lain, yaitu
  `message_id`, `conversation_id`, `seq`, `sender_id`, `sender_device`, `kind`, `created_at`,
  `reply_to`, `edited_at`, penanda hapus, dan `sender_key_id`, beserta `envelope` tersegel
  apa adanya, plus tanda baca dan tanda sedang menulis serta event game pada percakapan itu.
- `TopicKind::Room` memberi event anggota dan keadaan sebuah room, dan untuk room yang
  memang tidak mengklaim enkripsi ujung ke ujung berarti isinya juga.
- `TopicKind::User` memberi transisi presence sebuah akun tanpa melihat blokir maupun
  setelan `show_last_seen`, yang berarti section 180 dilanggar melalui pintu ini meskipun
  jalur baca presence sendiri menghormatinya. Yang tidak bocor hanyalah Invisible, karena
  `visible_state` sudah memproyeksikannya menjadi Offline sebelum penyiaran.

Cacat kedua yang menempel adalah biaya. Sebuah frame hanya dibatasi `MAX_FRAME_BYTES`,
sementara satu `Topic` hanya berbiaya sekitar 18 byte di wire, jadi satu frame dapat
menyebut puluhan ribu topic. Pada kode yang lama itu baru soal biaya di `Hub`; begitu
otorisasi benar-benar menanyai crate domain, ia menjadi pengali beban terhadap database lewat
satu frame yang dibayar satu tagihan rate limiter.

Perbaikannya sekarang ada di pohon dan diuji. Bentuknya: satu metode batch
`authorize_topics` pada trait `Dispatcher` yang sudah ada, bukan port ketiga, sehingga
pernyataan migo.md bahwa gateway bicara ke domain lewat tepat dua trait tetap benar; default
trait yang menolak segalanya sehingga implementor yang lupa gagal tertutup dan bukan terbuka;
`TopicRequest` yang tidak bisa membalas atau mempublikasikan sehingga tidak dapat dipakai
sebagai `ClientContext` secara tak sengaja; pemotongan daftar topic pada `MAX_SUBSCRIPTIONS`
sebelum domain ditanya, yang menutup pengali di atas; penolakan yang dituang ke daftar
`rejected` yang sudah ada dan tetap tanpa alasan, sehingga bukan anggota tidak dapat
dibedakan dari tidak ada, sesuai section 48; serta `AppDispatcher` di `migod` yang
memetakan Conversation ke `Messaging::is_participant`, Room ke `Roomkeeper::authorize` dengan
mask kosong, User ke akun pemanggil sendiri atau `Social::may_interact` dengan
`Interaction::LastSeen`, dan Unknown serta Game ke penolakan karena tidak ada yang pernah
menyiarkan ke sana.

Bagian 1 sebelumnya tetap mencantumkan `migo-gateway` dengan penanda `(8b)`, dan itu bukan
kelalaian melainkan bukti bahwa syaratnya kurang. Crate itu benar-benar lulus
`cargo build`, `cargo clippy --all-targets`, `cargo doc`, dan `cargo test`, yaitu keempat
hal yang bagian 1 minta, sementara cacat di atas lolos melewati keempatnya tanpa satu pun
berubah warna, sebab tidak ada test yang menanyakan apakah topic yang bukan milik pemanggil
ditolak. Aturan di bagian 9 nomor 2 memindahkan sebuah item ke selesai berdasarkan test
yang lulus, dan itu hanya sekuat pertanyaan yang test-nya berani ajukan. Karena itu barisnya
diberi penanda alih-alih dipindahkan: memindahkannya ke belakang dilarang aturan nomor 3,
sedangkan membiarkannya tanpa tanda akan membuat tabel mengatakan sesuatu yang tidak benar.
Penanda itu dicabut pada commit ini, dan bukan lebih awal: ia mengikuti perbaikan dan
test-nya, bukan mendahului keduanya. Test yang menutup keempat perilaku itu ada di tiga
kasus `migo-gateway/tests/gateway.rs` (dispatcher null, dispatcher yang hanya mengabulkan
topik sendiri, dispatcher yang mengabulkan segalanya dan diuji terhadap langit-langit) dan
lima kasus `migod/tests/migod.rs` yang menggerakkan `AppDispatcher` di atas `App` in-memory
sungguhan lewat domain nyata, sehingga setiap perilaku yang dituntut kepala suite gateway
kini punya test dan kode.

## 9. Aturan yang mengikat status ini

Diambil dari migo.md section 177, karena aturannya sendiri adalah bagian dari statusnya:

1. Status WAJIB diperbarui pada commit yang sama dengan perubahan kodenya.
2. Sebuah item hanya boleh ditandai selesai bila punya test yang benar-benar dijalankan CI.
3. Ketiga blok yang namanya memuat TEST BELUM DITULIS WAJIB kosong pada saat rilis, dan sebuah
   item hanya boleh berpindah keluar dari blok itu menuju selesai, tidak pernah sebaliknya.

## 10. SPEC opcodes di-v0.2.0 (dispatch layer)

Pada rilis ini, opcode SPEC (migo.md section 145) digabungkan ke schema dan dialihkan di
`AppDispatcher` (server/crates/migod/src/dispatch.rs). Setiap handler membangun `Caller`,
mendekode frame via `from_frame`, memanggil satu metode domain, lalu `reply`/`publish`.
Sub-modul per-domain yang menangani permintaan klien:

- `media.rs` — MEDIA_UPLOAD_BEGIN/STATUS/COMMIT/ABORT, MEDIA_FETCH_URL → `migo_media::Library`.
- `social.rs` — FRIEND_REQUEST, FRIEND_RESPOND, BLOCK_SET, RELATIONSHIP_LIST → `migo_social::Graph`.
- `notify.rs` — NOTIFICATION_ACK, NOTIFICATION_LIST → `migo_notify::Notifier`.
- `economy.rs` — GIFT_SEND, BALANCE_FETCH → `migo_economy::Treasurer`.
- `bots.rs` — BOT_REGISTER, BOT_COMMAND → `migo_bots::Bots`.
- `moderation.rs` — REPORT_CREATE, MODERATION_ACTION → `migo_moderation::Warden`.
- `federation.rs` — 14 opcode FED_* → `migo_federation::Mesh` (boundary: hello/auth periksa
  epoch, shard_map/directory jawab daftar peer, sisanya dekode lalu ack karena efek substansial
  nya mendarat di surface rooms/presence/messaging yang mesh hanya kirimkan).

Opcode s2c murni (FRIEND_EVENT, MEDIA_STATE_EVENT, ECONOMY_EVENT, BOT_EVENT, MODERATION_EVENT)
tidak punya arm handler klien: server memublikasikannya, bukan memintanya.

Test integrasi per-domain ada di `server/crates/migod/tests/spec_*.rs` (media, social, notify,
economy, bots, moderation, federation) yang menyusun service in-memory sungguhan dan menegaskan
perilaku yang handler andalkan (dompet nol, kirim gift mengurangi koin, daftar relasi berisi
target, ack membalik unread, dsb). `migod/tests/migod.rs` dan `migo-gateway/tests/gateway.rs`
disesuaikan agar dispatcher 12-handle dan daftar push-only s2c yang baru lolos gate.

Catatan jujur: handler federasi memanggil metode batas keamanan mesh, bukan router aplikasi —
sesuai desain `Mesh` (section 169). Alur server-ke-server penuh (handshake, forwarding antar
node) memerlukan transport mesh terpisah yang belum diarahkan lewat gateway klien; arm dispatcher
ada dan terkompilasi, namun lalu lintas FED_* dari soket klien akan ditolak gate autentikasi
sebagai `auth: Server`.

## 11. Perbaikan lockout KEY_PUBLISH (v0.2.1)

Laporan pengguna: "The server rejected the request." pada pendaftaran baru. Akar masalah:
`KEY_PUBLISH` (harga 20) ditagih dua kali ke bucket endpoint yang SAMA — sekali per frame di
tepi gateway (`migo-gateway` `charge_or_reject`), lalu lagi di dalam service pemiliknya
(`migo-keys` `Keys::charge`). Bucket endpoint akun baru (tier New, umur < 7 hari) hanya memuat
25 token, sehingga tagihan kedua (20) tidak pernah terbayar — setiap akun baru ditolak
`RATE_LIMITED` pada koneksi pertamanya, sebelum satu pesan pun terkirim. Opcode lain berharga

> = 13 (ROOM_JOIN, GIFT_SEND, REPORT_CREATE) terkena tembok yang sama.

Perbaikan: tagihan milik service kini mendarat di bucket terpisah
(`BucketKey::endpoint_write_of_account`, scope Endpoint dengan tail `/write`), sehingga tepi
gateway dan service masing-masing membayar bucket sendiri; ukuran bucket tidak berubah.
`Policies::validate` kini menuntut permukaan Account mampu membayar plafon biaya DUA kali
(karena tepi dan service sama-sama menagih account), dan limiter menolak boot bila tidak.
Test regresi `a_probationary_account_can_pay_for_its_first_publish_twice` di
`migo-ratelimit/tests/limiter.rs` menyematkan kedua invarian itu, terverifikasi end-to-end:
register → connect → KEY_PUBLISH → SUBSCRIBE lulus pada konfigurasi bawaan.

Catatan jujur: cacat ini sudah ada sejak layer charge gateway dan service digabungkan dan lolos
karena test gateway memakai dispatcher palsu sementara test domain menggerakkan service secara
langsung — tidak ada test yang menjalankan kedua layer di atas limiter sungguhan dengan akun
tier New. Pelajarannya tercatat di test regresinya.

## 12. Dua lapisan tidak lagi menagih bucket yang sama (v0.2.4)

Perbaikan di bagian 11 memisahkan bucket endpoint, tetapi permukaan **account** masih ditagih
dua kali untuk satu permintaan: tepi gateway menagih frame, lalu service pemiliknya menagih
kerja yang sama. Akibatnya satu opcode berharga dua kali lipat dari yang dinyatakan registry.
Pada `KEY_PUBLISH` (harga 20) itu berarti 40 dari 50 token akun tier `New` habis hanya untuk
menerbitkan kunci saat connect — sehingga aksi pertama pengguna sesudahnya (membuat percakapan,
harga 10) ditolak `RATE_LIMITED`. Akun baru bisa masuk, lalu tidak bisa berbuat apa pun.

Sekarang setiap lapisan menagih bucket sendiri: tepi pada `BucketKey::account` /
`endpoint_of_account`, service pada `BucketKey::account_write` / `endpoint_write_of_account`.
Ukuran bucket tidak berubah dan `Policies::validate` kembali menuntut plafon satu kali, karena
tidak ada lagi bucket yang ditagih ganda. `migo-auth` tetap memakai permukaan kanonik: jalurnya
REST, dan tepi REST hanya menagih bucket IP, jadi tidak ada tabrakan.

Terverifikasi end-to-end pada konfigurasi bawaan, dua klien SDK sungguhan terhadap `migod`
in-memory: dua akun mendaftar, keduanya connect, satu membuat percakapan langsung, mengirim
pesan terenkripsi, dan klien kedua menerima serta mendekripsinya. Ini jalur produk yang
sebenarnya, dan inilah pertama kali ia dijalankan utuh — sebelum perbaikan ini langkah
"membuat percakapan" selalu gagal.

Test regresi `a_probationary_account_can_connect_and_then_still_do_something` di
`migo-ratelimit/tests/limiter.rs` menyematkan invariannya: kunci tepi dan kunci write berbeda
pada kedua permukaan, dan seluruh urutan pembuka klien nyata (KEY_PUBLISH, SUBSCRIBE,
CONVERSATION_CREATE, MESSAGE_SEND) terbayar pada kedua lapisan untuk akun yang baru mendaftar.

Catatan jujur: biaya registrasi memang menghabiskan seluruh bucket anonim (satu pendaftaran per
~5 detik per /24) — itu perilaku anti-spam yang disengaja dengan override
`auth.registration_cost` untuk pengembangan lokal, bukan cacat.

## 13. Yang SPEC kini bekerja: perintah bot dan transport mesh (v0.2.5)

Dua item yang di bagian 10 diakui masih setengah jadi kini memiliki implementasi nyata dengan
test yang menutupnya.

BOT_COMMAND tidak lagi decode-dan-ack. `migo-bots` mendapat metode `command` pada trait `Bots`
beserta port `Webhook` yang diisi composition root dengan klien HTTPS sungguhan
(`ReqwestWebhook`, reqwest + rustls, batas waktu 5 detik) dan dengan palsu perekam di test.
Aturannya milik crate: bot harus ada dan aktif (bot jeda membaca sama dengan bot tak dikenal,
§161), webhook wajib terdaftar — tanpa itu perintah ditolak `VALIDATION_FAILED` alih-alih
ditelan diam-diam, karena pengguna menunggu balasan yang tidak akan pernah datang — dan
payload JSON memuat identitas pemberi perintah agar bot bisa menjawab lewat kanal yang sama.
Enam test baru menutup delivery, argumen yang berbentuk merusak tetap menjadi satu string,
penolakan tanpa webhook, opasitas bot jeda, satu error opak untuk webhook mati, dan harga 2
yang tetap dipungut walau ditolak.

Transport mesh kini ada: `migod::mesh::MeshTransport`, dua tugas tokio yang melengkapi batas
keamanan `Mesh`. Listener menerima koneksi TCP dan menjalankan sisi server handshake
(`FED_HELLO` dua arah, `FED_AUTH` membawa `NodeProof` sebagai `signed_at || signature`,
diverifikasi lewat `Mesh::authenticate` yang menolak peer tak dikenal, jeda, atau diblokir
sebelum tanda tangan diperiksa). Runner menguras outbox (`Mesh::due`) dan mengirim setiap
event sebagai `FED_FORWARD` bernomor sequence per link; penerima menjawab `FED_ACK` berupa
watermark kumulatif, yang dipetakan ke `Mesh::mark_delivered`; kegagalan kembali sebagai
`Mesh::mark_failed` dengan backoff eksponensial yang sudah ada di crate. Frame dibingkai
u32 big-endian + MWP/1 tanpa JSON, sesuai section 169; replay dijatuhkan tanpa ack dan gap
meruntuhkan link (`check_sequence`); ping-pong memakai opcode PING untuk dua arah. Event
yang tiba di-route: `FED_ROOM_EVENT` di-publish ke hub gateway sehingga sesi yang berlangganan
room menerima persis seperti event lokal (bukit terakhir yang lengkap), sementara digest
presence, call relay, subscribe, rotasi key, health, dan error divalidasi, dihitung di metrik,
dan dicatat — port ingest presence/panggilan adalah langkah berikut, dan keterbatasan itu
ditulis di sini alih-alih disembunyikan di balik ack.

Testnya: tiga test unit di `migod/src/mesh.rs` yang menghubungkan dua `MeshService` sungguhan
via duplex (handshake penuh, delivery, replay dijatuhkan, gap meruntuhkan link) dan satu test
integrasi `migod/tests/federation_link.rs` yang mengikat listener loopback dan mengalirkan
event dari outbox node B ke ingest node A melalui TCP nyata dengan runner sungguhan.

Catatan jujur: jam proses uji memakai skala `Timestamp` ber-epoch kustom; upaya pertama
transport membaca jam UNIX untuk stamp sesi sehingga setiap bukti handshake tampak meleset
lima puluh tahun dan ditolak — cacat itu ditemukan oleh test integrasinya sendiri dan
diperbaiki dengan menyerahkan `Clock` milik composition root ke transport, bukan membaca jam
host di dalamnya. Konfigurasi listener mesh adalah `node.mesh_bind` (opsional; tanpa itu
listener mati dan runner tetap berjalan, no-op selama allow-list kosong); listener mesh
tetap milik segmen internal dan tidak boleh menghadap internet umum (section 169). TLS di
depan listener adalah langkah deployment berikutnya; framing binary-nya sudah versi wire
yang sama.

## 14. Image CAPTCHA sebagai standar (v0.2.6)

Keputusan terdokumentasi "deliberately not an image captcha" dibalik dengan alasan yang
dicatat jujur di migo.md: kode numerik enam-digit yang dibawa teks di respons selesai
dipecahkan oleh script yang sama yang membaca body, sehingga gerbangnya hanya memperlambat
satu request. Sekarang `migo-captcha` merender tantangan sebagai PNG server-side: 5–6
karakter alfanumerik huruf besar dari alfabet tanpa I/O/S/0/1/5, tiga font TTF yang
di-embed, rotasi/skala/jitter per karakter, latar ber-dot dan ber-speckle, wobble per-baris,
dan tepat satu kurva interferensi Catmull-Rom yang knot-nya struktural di pita tinta setiap
karakter sehingga dijamin melintasi semua karakter dan tidak pernah lurus. Yang disimpan
hanya tag HMAC jawaban (kolom `tag` migrasi 0002 sudah berbentuk itu sejak awal); verifikasi
constant-time, case- dan whitespace-insensitive, sekali-jalan lewat `consume` atomik di
store; TTL 120 detik. Mode `image_alt` adalah jalur aksesibel: tantangan baru dengan kode
acak berbeda dan render lebih lunak — tetap gambar, bukan bypass. Konfigurasi di
`CaptchaConfig` (enabled/length/ttl/accessible_mode/noise/ukuran) tervalidasi startup dan
default-nya ON; `captcha.enabled = false` setara threshold `None`.

Client: web menampilkan `<img>` responsif dengan tombol refresh dan "Easier challenge",
input ternormalisasi (uppercase, tanpa whitespace), 79/79 test web dan 88/88 SDK lulus;
desktop egui menampilkan tantangan sebagai texture dengan alur fetch edge-triggered dan
normalisasi yang sama, 18/18 test lulus. Server: 16 test crate captcha (alfabet, keunikan
gambar dan tag antar-issue, determinisme dari seed, PNG valid berukuran konfigurasi, mode
alt berbeda, benar/salah/kadaluarsa/replay, dua jawaban berlomba hanya satu menang, view
tak memuat jawaban), auth-flow mem-pin wire (gambar PNG, tanpa field `question`, mode alt
berbeda, mode tak dikenal ditolak), dan 80 suite workspace hijau. E2E dua akun
register-connect-pesan terenkripsi tetap lulus.

Catatan jujur: pintu `issue_for_test` di balik fitur `test-internal` adalah satu-satunya
cara test integrasi menyelesaikan tantangan gambarnya sendiri — fitur itu hanya dinyalakan
dev-dependencies dan tidak pernah ada di build produksi. Rate limit IP anonim di route
captcha tetap lapisan pertama (biaya bootstrap per /24), captcha lapisan kedua, threshold
kegagalan lapisan ketiga; captcha tidak pernah menjadi satu-satunya pertahanan.

## 15. Fase 1: data plane media, scan inline, penerbit event, BandwidthMode (v0.3.0)

Audit menyeluruh terhadap migo.md menemukan mesin-mesin tanpa setir: transport dan domain
kokoh tetapi produknya terputus. Fase ini menyambungkan empat setir terpenting.

Data plane media (section 168) kini ada: satu PUT dan satu GET di bawah `/media/{key}` pada
migo-api, dipasang hanya ketika backend storage adalah filesystem (backend S3 melayani
bytenya sendiri dan proses menjawab 404). PUT menegakkan `media.max_upload_bytes`, GET
menyajikan content type dari magic byte lewat sniff dan menolak byte yang scanner tolak,
keduanya menolak key yang mencoba keluar dari root media. Port `MediaFiles` didefinisikan
di migo-api dan diimplementasikan migod atas FsStorage sehingga migo-media tetap tidak
pernah melihat HTTP. Empat test api baru menutup round-trip, traversal, langit-langit, dan
penolakan HTML polyglot.

Scan media kini inline pada commit (bug deadlock): media server-readable tidak lagi
diparkir `Pending` menunggu scanner yang tak pernah dikomposisi — verdict diambil dari
sniff atas head yang sudah dibaca commit, HTML/SVG polyglot ditolak sebelum menjadi baris,
dokumen tanpa magic (teks) tetap sah sebagai Clean, dan deployment dengan scanner yang
lebih ketat dapat menurunkan verdict lewat `record_scan`. Avatar dan media room kini benar-
benar tersaji ke pengguna lain; test lama yang mem-pin perilaku Pending ditulis ulang ke
perilaku baru.

Penerbit event yang hilang kini hidup, semuanya dari dispatcher yang memegang konteks
koneksi: FRIEND_REQUEST/FRIEND_RESPOND menerbitkan `FRIEND_EVENT` ke topik User penerima
plus notifikasi (baris inbox lewat Notifier + pencerminan realtime `NOTIFICATION_EVENT`
dengan coalescing per penerima) memakai `Notice` dari migo-social yang sebelumnya dibuang;
GIFT_SEND menerbitkan `ECONOMY_EVENT` ke topik pengirim dan `NOTIFICATION_EVENT` ke
penerima, dengan baris inbox dari Announcer ekonomi yang kini diikat ke Notifier nyata
(sebelumnya Silent); MEDIA_UPLOAD_COMMIT menerbitkan `MEDIA_STATE_EVENT` ke topik
Conversation dengan coalescing per objek. Inbox notifikasi tidak lagi kosong secara
struktural.

BandwidthMode (section 75) kini dibaca gateway dari HELLO, disimpan pada session handle,
diekspos `ClientContext::bandwidth_mode`, dan dipakai dispatcher presence — kadensi
heartbeat menurut mode, bukan default untuk semua.

Ditunda dengan sadar ke fase berikutnya: permukaan wire baru (PROFILE_UPDATE, edit pesan,
reaksi, room admin, games start, economy baca, discovery), voice note perekaman/pemutaran,
dan calls penuh (§165/§166/§180). §177 migo.md disinkronkan dengan kenyataan: blok SCHEMA
dan SPEC kini membedakan yang sudah menyentuh kabel dari yang benar-benar masih dokumen.

## 16. Fase 2 batch pertama: delapan opcode baru menyentuh kabel (v0.3.1)

Delapan opcode baru masuk registri beserta handler-nya, masing-masing mengikuti aturan
alokasi §146 (dalam range domainnya) dan tabel §145 (barisnya ditambahkan):

- **111 PROFILE_UPDATE**: caller mengubah profil dan pengaturan privasinya sendiri.
  Patch semantics: absent = biarkan. Avatar bio, nama tampilan, tahun lahir, dan empat
  pengaturan visibilitas kini bisa diubah lewat wire — sebelumnya semua akun terkunci
  di default selamanya.
- **118 SUGGESTIONS** dan **119 SEARCH**: discovery orang dari graph sosial. Suggestions
  di-resolve ke kartu profil (sehingga blokir diam-diam menghapus saran alih-alih membocorkan
  nama), search adalah prefix-match pada username/nama tampilan dengan opt-in searchable.
- **40 MESSAGE_EDIT**: mengedit envelope terenkripsi di tempat dengan seq yang sama.
  Hanya pengirim; `edited_at` terekam di store. Envelope adalah bytes — teks tidak pernah
  kelihatan di server.
- **41 REACTION_SET** dan **42 REACTION_EVENT**: reaksi sebagai pesan kind Text dengan
  discriminator Reaction di dalam ciphertext (mengikuti kindForContent SDK), plus event
  s2c Coalescable untuk pembaruan real-time.

Semua handler baru berada di dispatch (profile.rs untuk 111/118/119; inline di dispatch.rs
untuk 40/41). AppDispatcher kini memegang handle store (layer 2) untuk jalur update profil.
Messaging crate mendapat method `edit` + meter `migo_messaging_edits_total`.

Opcode yang masih SPEC (rooms admin, games start/view, economy baca, calls) menyusul di
batch berikutnya — schema dan service-nya sudah siap menampung.

## 17. Fase 2 batch 2 + Fase 3 batch 1: 14 opcode server + navigasi web (v0.3.2)

Batch 2 server: empat belas opcode baru menyentuh kabel, semuanya dengan handler di tiga
modul dispatch baru (rooms_admin, games_admin, economy_read):

- **Rooms admin (85-89)**: ROOM_CREATE (caller jadi Owner; conversation_id dari authorize
  karena create hanya mengembalikan RoomSummary), ROOM_ROSTER (halaman, role tertinggi
  dulu), ROOM_ROLE_SET, ROOM_UPDATE (topic kosong = hapus, sesuai bacaan service), dan
  ROOM_ARCHIVE. Fanout room lewat publish_rooms yang kini pub(crate).
- **Games (183-186)**: GAME_START (slug → GameKind via tiga nama tertutup; tanpa lawan di
  wire, service yang menolak jumlah pemain yang salah), GAME_VIEW (GameView → GameViewWire
  dengan render papan per-kind), GAME_ABANDON, GAME_CATALOGUE.
- **Economy read (163-167)**: GIFT_CATALOGUE (listings() → nama slug), LEDGER_HISTORY
  (statement coins; magnitude tanpa tanda, reason = arah), PROGRESSION, BADGES,
  LEADERBOARD ("xp" → Global AllTime; "reputation" ditolak karena board-nya belum ada).

Batch 1 web: kerangka navigasi dengan lima tab + domain SDK baru:

- **Tab rail**: rail vertikal di desktop, bottom bar di mobile, aria-current untuk screen
  reader. Chat view tidak tersentuh — hanya jadi salah satu tab.
- **SDK**: SocialDomain (friend request/respond/block/list/suggest/search/onFriendEvent),
  EconomyDomain (balance/gift/catalogue/ledger/progression/badges), updateProfile di
  ProfileDomain, listNotifications + acknowledgeNotifications di NotificationsDomain.
  13 test baru SDK.
- **Panels**: FriendsPanel (permintaan + saran + cari), NotificationsPanel (inbox + mark
  all read), ProfilePanel (edit nama/bio/privasi), DiscoverPanel (browse room + join →
  hands-off ke chat). 6 test web baru.

Verifikasi: 80 suite server, 102 test SDK, 85 test web, 14 gate CI, e2e dua-akun lulus.

## 18. Fase 3 batch 2: pengalaman chat lengkap + media (v0.3.3)

Chat web naik dari slice teks minimum ke pengalaman lengkap, semua di atas SDK yang sudah
ada — tak ada perubahan protocol:

- **Tombstone hapus**: onDeletion menandai pesan `deleted`; baris tetap di posisinya dengan
  gaya redup miring dan tanpa konten. Tombol hapus di hover pesan sendiri.
- **Read marker dua arah**: onReceipt merekam watermark `readUpTo`; pesan sendiri
  menampilkan ✓ (terkirim) / ✓✓ (dibaca).
- **Nama + avatar pengirim di grup**: per-run (nama hanya saat ganti pengirim), batch-fetch
  via useProfiles.
- **Reply**: tombol Reply di hover → bar pratinjau di composer → `replyTo` di SendOptions →
  snippet ter-kutip di gelembung (target di-resolve in-thread, [deleted] bila hilang).
- **Pratinjau pesan-terakhir di daftar percakapan**: lastMessage di-dekripsi via ingest
  (pola yang sama dengan desktop), "Nama: isi" untuk grup / isi untuk direct, placeholder
  per kind (📎/🎤/🎉), [deleted] untuk tombstone, fallback ke subtitle lama.
- **Label enkripsi dari EncryptionMode** (bukan lagi dari kind): EndToEnd → 🔒, Transport →
  "server dapat membaca untuk moderasi", None → tanpa label. Tidak pernah ditebak.
- **Muat lebih lama**: tombol "Load earlier messages" dengan paging backwards via
  sync.fetch + ingest; replay awal tetap maju dari seq 1 karena membangun ulang state
  receiver sender-key (hanya in-memory di SDK).

Media SDK + web:

- **MediaDomain** (SDK): begin/uploadBytes(PUT HTTP)/status/commit(SHA-256 digest)/abort/
  fetchUrl, plus convenience upload() dan download() dengan cache URL per sesi. 12 test.
  Catatan penting: MediaKind mengikuti diskriminan server (0=Avatar, 1=Image, 2=Video,
  3=Audio, 4=VoiceNote, 5=Document) — bukan urutan di teks tugas.
- **Lampiran gambar di web**: tombol 📎 di composer → unggah → kirim MediaRefContent;
  render `<img>` maks 300×300 dengan placeholder dan lightbox klik-untuk-zoom. Mime dari
  pengirim tidak pernah dipercaya untuk render (test XSS tetap hijau).
- **Unggah avatar**: di panel profil → unggah media kind Avatar → updateProfile.

Verifikasi: 80 suite server, 114 test SDK, 108 test web, 14 gate CI, e2e dua-akun lulus.

## 19. Fase 3 batch 3: voice note lengkap + gifts + games + rooms chat (v0.4.0)

Voice note (§179) kini pengalaman penuh di web:

- **Rekaman**: tombol 🎤 di composer → getUserMedia → MediaRecorder (webm, fallback default),
  indikator titik merah berdenyut + timer monospace, batas keras 5 menit (dengan clamp durasi),
  teardown satu jalur untuk stop/unmount/cancel (track dihentikan, AudioContext ditutup).
  Waveform ~50 bar dihitung dari sampel amplitudo puncak per 100ms (AnalyserNode).
- **Pengiriman**: unggah via MediaDomain kind VoiceNote + durationMs, kirim VoiceNoteRefContent
  (placeholder key/nonce 32+12 byte, pola yang sama dengan lampiran gambar).
- **Pemutaran**: tombol play/pause + bar waveform SVG currentColor + label M:SS, progress
  fallback saat webm melaporkan Infinity, re-fetch URL sekali saat kedaluwarsa, cleanup penuh.
  Mime dari pengirim tidak pernah dipercaya. 18 test baru (formatDuration, downsampleWaveform,
  pickRecorderMimeType, normalisasi mime, konstruksi konten, cap 5 menit, render).

Gifts panel: saldo (coins + points), grid katalog dengan tombol kirim, penerima dari daftar
teman + pencarian username, ledger 10 transaksi terakhir, kartu progress dengan bar XP
(level, xp_into_level / xp_for_next_level). 9 test.

Games di chat: tombol 🎮 di header (Group/Room saja) membuka popover katalog; game dua pemain
disabled dengan alasan (GAME_START tak bisa menyebut lawan); baris game sebagai system line
terpusat (bukan gelembung) dengan gaya selebrasi saat selesai; input tebakan inline untuk
game single-player (1-100) dengan feedback higher/lower/correct. 13 test.

Rooms di chat: room yang di-join muncul di daftar percakapan dengan glif # + jumlah anggota;
header room menampilkan online_count dan topic; RoomsProvider menyimpan metadata room
per akun di IndexedDB. 4 test.

SDK: GamesDomain mendapat getCatalogue(), startGame(), getView() + 5 test.

## 20. Calls: signaling penuh 1-on-1 (v0.5.0)

Opcode call 224-238 (§145) masuk registri beserta 15 struct wire (§165) dan crate
`migo-calls` (state machine signaling), membawa total ke 100 opcode dan 168 struct.

**Server (migo-calls)**: Callkeeper dengan 11 metode — siklus hidup
Ringing-Connecting-Connected-Ended dengan enam alasan Ended (§180), invite ber-idempotensi
call_id dari client (retry tidak membunyikan dua kali; reuse dengan payload berbeda
dijawab IDEMPOTENCY_MISMATCH), relay SDP dan ICE tersegel yang hanya membaca header routing
tanpa pernah membuka byte tersegel (§165: "Server meneruskan blob tersegel dan tidak
mengurainya"), gate keanggotaan+block dari store (gagal ke arah menolak), sweep invite
kedaluwarsa di dalam invite (tanpa background task), TURN dari config (daftar kosong untuk
sekarang), SFU group (237-238) dijawab FEATURE_DISABLED sampai deployment SFU tersedia.
21 test.

**Dispatch**: 13 handler di dispatch/calls.rs. Invite menerbitkan CALL_INVITE_EVENT ke topik
User callee; answer/decline/cancel/end menerbitkan CALL_STATE_EVENT ke topik User pihak
lainnya; SDP/ICE relay diteruskan ke topik User perangkat tujuan; sweep dijalankan di dalam
invite. AppDispatcher kini 14 domain.

**SDK**: CallsDomain dengan signaling penuh (invite/answer/decline/cancel/end/sendSdp/
sendIce/getTurnServers/reportStats) dan empat listener (onIncomingCall/onCallState/onSdp/
onIce) yang memfilter relay untuk perangkat ini. 12 test.

**Web**: CallManagerProvider yang memiliki RTCPeerConnection — offer→invite, accept→answer

- SDP, ICE batching dengan linger 250ms (caller menunggu sampai answer menyebut device
  callee), jendela reconnect 30 detik → Network, mute, teardown di setiap jalur keluar,
  auto-decline saat sibuk, CALL_STATS setup-time. CallOverlay layar penuh dengan keenam state
  §180 (termasuk Degraded) dan sebab Ended yang dibedakan. Tombol telepon/video di header chat
  direct. 18 test.

Placeholder sealing (32-byte nol + 12-byte nol) untuk SDP/ICE — pola yang sama dengan
lampiran gambar; enkripsi media E2E penuh adalah tugas tersendiri yang membutuhkan
integrasi dengan session-crypto SDK.

## 21. Audit pasca-v0.5.0: perbaikan gate, push, TURN, avatar, dan sembilan bug call (v0.5.1)

Audit menyeluruh menemukan mesin signaling kokoh tetapi lingkungannya bolong. Semua P0
dan P1 diperbaiki:

**Server:**

- **Gate izin panggilan**: `StoreCallGate` kini memegang `SharedSocial` dan memanggil
  `may_interact(Interaction::Call)` — callee yang mengatur "nobody can call" tidak lagi
  dibunyikan. Gagal ke arah menolak ( Blocked, tanpa baris tersimpan).
- **Push incoming call**: handler invite mengirim notifikasi `IncomingCall` (actor=caller,
  subject=call_id) ke callee via notifier — offline callee mendapat wake-up dan baris inbox.
- **CallsConfig**: `ring_ttl_ms` (5-120 detik, divalidasi) + `turn_servers` kini
  operator-configurable via `MIGO_CALLS__*`. `turn_servers()` mengembalikan dari config.
- **Feature bit CALLS**: `FEATURES` kini `migo_protocol::features::CALLS` (bit 17) —
  klien spec-conforming melihat tombol call.
- **avatar_media_id di wire**: `UserProfile` menambah field optional `avatar_media_id`;
  kedua proyeksi profile mengisinya dari `ProfileCard` yang sudah membawanya.

**Web (9 bug call diperbaiki):**

- **Phantom ring**: `handleStateEvent` kini menangani Ended untuk ring yang belum diterima —
  batal/kedaluwarsa menghentikan ring dengan kartu "Missed call".
- **Timeout caller**: timer lokal dari `expiresAt` auto-cancel dengan NoAnswer.
- **Dedup invite**: redelivered invite diabaikan (bukan auto-decline Busy).
- **Double-click**: `startingRef` synchronous menutup race; tidak ada mic/pc leak.
- **Network death**: `endCall(Network)` dikirim ke server; `beforeunload` fire-and-forget.
- **STUN/TURN**: `iceServersForCall` selalu menyertakan STUN fallback + TURN dari config.
- **Blocked ≠ Declined**: status 3 menampilkan "Unavailable" (bukan "Declined").
- **Avatar display**: `use-profiles` me-resolve `avatarMediaId` via media URL cache;
  avatar tampil di sidebar, header chat, overlay call, daftar percakapan, friends panel,
  dan update langsung setelah unggah.

Verifikasi: 83 suite server, 132 test SDK, 187 test web, 14 gate, doc-link bersih,
e2e dua-akun lulus.

## 22. Tema neon dark + integrasi penuh + port 19992 (v0.6.0)

**Tema neon dark**: seluruh globals.css di-retheme ke estetika neon messenger — latar
`#0a0a12` (hitam pekat), aksen neon cyan `#00d4ff` / hijau `#00ff88`, radius 6px (kompak),
font 14px, glow effects pada hover/focus, backdrop-filter blur pada tab rail dan modal,
scrollbar cyan-tipis, bubble outgoing dengan gradient cyan, tab rail semi-transparan.
PWA manifest theme_color disinkron.

**SDK gap methods** (6 baru + 9 test): `rooms.create`, `rooms.getRoster`,
`economy.getLeaderboard`, `messaging.editMessage`, `messaging.sendReaction`,
`social.listAllRelationships` (semua kind termasuk blocks). Catatan jujur: `unblockUser`
tidak ada karena wire `FriendTarget` tak punya field unblock dan `BLOCK_SET` handler
server hanya memanggil `Graph::block` — tak ada jalur wire untuk unblock.

**Web client integrasi penuh** (7 komponen baru, 12 dimodifikasi, 33 test baru):

- **SettingsPanel**: daftar device/session dengan revoke per-session + sign out others,
  form ganti password yang mempertahankan grant.
- **UserProfileModal**: klik avatar/nama di chat header atau friends → popup dengan
  avatar, bio, level+XP bar, badges, tombol Block/Send Message.
- **RoomInfoPanel**: roster room dengan badge role (Owner/Admin/Member), Leave Room.
- **PresencePicker**: dropdown Online/Away/Busy/Invisible + custom status (100 char)
  di sidebar footer.
- **GiftPicker**: tombol 🎁 di composer → katalog inline 6 gift teratas dengan
  penerima auto-untuk Direct.
- **New-conversation search**: 300ms-debounced `social.search` menggantikan paste-ID,
  plus quick-pick dari friends.
- **Message edit + reaction bar**: hover → Edit (inline editor dengan re-seal),
  👍❤️😂 reaction bar, mark `edited`.
- **Leaderboard** di gifts panel, **badges** di profile panel, **coin balance** di
  sidebar header, **blocked section** di friends panel.
- Tab rail kini 7 tab: Chats, Friends, Notifications, Discover, Gifts, Profile, Settings.

**Desktop client** (tema neon + fitur): tema dark neon cyan di theme.rs (13 test),
navigasi rail dengan 3 tab (Chat/Friends/Settings), friends panel dengan search +
accept/decline + presence, settings panel dengan device list + revoke + theme toggle,
chat dengan avatar peer + label enkripsi + unread badge. 31 test desktop total.

**Deployment port 19992**: semua referensi 19991 → 19992 (package.json, serve.mjs,
Dockerfile.web, docker-compose, infra-audit). Web client berjalan di `0.0.0.0:19992`,
migod di `0.0.0.0:8080`, keduanya terverifikasi via IP publik 152.53.102.150.

Verifikasi: 83 suite server, 141 test SDK, 220 test web, 31 test desktop,
14 gate CI, e2e dua-akun lulus, tsc --noEmit bersih.

## 23. Redesign layout: top navigation, dark/light theme, login card baru (v0.7.0)

**Web client** — navigasi vertikal digantikan horizontal:

- **TopNav** menggantikan TabRail: bar horizontal 48px dengan brand ◆ Migo di kiri,
  7 tab fitur (Chat, Friends, Alerts, Discover, Gifts, Profile, Settings) di tengah,
  chip akun (avatar + nama) + toggle tema di kanan. Tab aktif ber-underline accent
  dengan glow. Mobile (≤768px): header 44px hanya brand+akun+toggles, navigasi fitur
  pindah ke bottom bar 52px.
- **Dark/light theme**: seluruh globals.css direstrukturisasi ke dua set CSS variable —
  `:root` (light: putih panel, biru accent #0077e6, bubble putih) dan
  `[data-theme="dark"]` (near-black, neon cyan #00d4ff, bubble gelap). Zero hardcoded
  color di luar blok variable. Toggle 🌙/☀️ persist di localStorage; pre-paint script
  mencegah flash tema salah. Auth screen juga punya toggle fixed top-right.
- **Login/register card**: brand mark 48px glowing + nama 28px bold di atas (bukan
  inline kecil), spacing lebih lega (32px padding), card 420px radius 12px.
- **Sidebar footer**: bar status dengan saldo koin + presence dot berwarna.
- 226 test web (+6 baru: 5 TopNav, 4 theme, -3 TabRail).

**Desktop client** — rail kiri digantikan top bar:

- **Top bar**: ◆ Migo brand di kiri, tab Chat|Friends|Settings horizontal di tengah,
  connection dot + toggle tema (☀/🌙) di kanan. Panel type `egui::Panel::top`.
- **Theme toggle** sekarang persist di settings file (serde `Theme`), kedua toggle
  (top bar dan settings pane) lewat satu jalur deferred `theme_choice`.
- **Chat header**: 🔒 encryption label lebih prominent (SMALL size, positive/warning
  colored), members pill, unread badge di kanan.
- 33 test desktop (+2 baru: theme round-trip, old settings file compatibility).

Verifikasi: 83 suite server, 141 test SDK, 226 test web, 33 test desktop, 14 gate CI,
e2e dua-akun lulus, deploy live di port 19992.

## 24. Redesign menyeluruh: satu design system lintas platform (v0.8.0)

**Migo Design System** — sumber kebenaran kanonik baru:

- `shared/design/tokens.json`: warna (light/dark), tipografi, spacing 4px, radius,
  elevation, ikon (16/20/24px, stroke 1.75), touch target 44px, motion 120/180/240ms,
  z-index, breakpoint. Diterapkan ke tiga client: web (CSS custom properties +
  skala baru --sp-_/--fs-_/--motion-_/--z-_), Android (Theme.kt palet Migo +
  MigoExtra CompositionLocal), desktop (theme.rs — palet light kini biru kanonik
  #0077e6, bukan hijau). `docs/design-system.md` mendokumentasikan; route `/design`
  merender sistem sebagai dokumentasi hidup.

**Web client** — AppShell tiga komposisi, sepuluh section:

- **AppShell** menggantikan TopNav: rail penuh (≥1024px), rail ikon (768–1023px),
  header 44px + bottom bar lima slot Home/Chats/Rooms/Space/More (<768px; More =
  bottom sheet). Thread terbuka melipat header global. Prefers-reduced-motion dihormati.
- **Section baru**: Home (dashboard realtime: saldo $MIG, chat terbaru, trending
  rooms, suggestions, digest alerts, top XP), Rooms (direktori + kategori + sort
  Popular/New + join bersama via useJoinRoom), Space (feed aktivitas: inbox +
  ledger gift + event live, filter Social/Rooms/Games/Economy), Search (terpadu:
  people+rooms di wire, chats lokal, debounce 300ms, recent searches di localStorage),
  Wallet (saldo MIG, progression, badges, gift shop + kirim gift, ledger, leaderboard).
- **$MIG token reference**: `$MIG` word-bounded di teks pesan jadi chip yang membuka
  Wallet (TokenText, pre-test regex murah).
- **Tema Light/Dark/System**: pilihan system mengikuti prefers-color-scheme lewat
  matchMedia + ThemeFollower; script pre-paint me-resolve 'system'.
- **Komponen baru**: icons.tsx (31 ikon SVG stroke satu keluarga — menggantikan glyph
  emoji), BottomSheet, ContextMenu (right-click desktop / long-press 450ms mobile),
  Skeleton, EmptyState, ErrorState, conversationTitle helper.
- **Restyle**: composer (ikon attach/gift/mic/send), sidebar (header section Chats),
  friends (context menu + aksi Message), settings (Appearance + About + link /design),
  notifications. Discover & Gifts panel dilebur ke Rooms & Wallet.
- 235 test web (+9: AppShell 7, theme 2, wallet-badge; TopNav/Gifts dihapus).

**Android client** — paritas penuh, bottom navigation:

- **Core domain baru**: Social.kt (search/suggestions/relationships/friend ops/block
  - FRIEND_EVENT), Economy.kt (balance/ledger/progression/badges/leaderboard/catalogue/
    sendGift), Notifications ditambah listNotifications/acknowledgeNotifications
    (watermark id 6-byte). Semua terwire ke MigoClient (+aksesor social/economy +
    onFriendEvent bridging reconnect).
- **UI**: shell bottom bar lima slot + sheet More, HomeScreen (hero saldo MIG, quick
  actions, recent chats, trending, suggestions, digest), RoomsScreen (search
  debounce + direktori + join), SpaceScreen (filter kategori), FriendsScreen (requests/
  friends/suggestions + search), SearchScreen, WalletScreen (saldo/level/badges/gift
  picker/ledger/leaderboard), AlertsScreen (mark all read), ProfileScreen. AppState
  bertambah 7 holder state; AppViewModel bertambah ~20 action + debounce search/rooms.

**Desktop client** — paritas penuh, sembilan place:

- **Net layer**: Command/Event baru (Rooms/JoinRoom/Notifications/AcknowledgeAlerts/
  Wallet/SendGift/SearchPeople/Suggestions/StartDirectById) + 13 handler frame
  (RoomList/RoomJoin/NotificationList/NotificationEvent/BalanceFetch/LedgerHistory/
  Progression/Badges/Leaderboard/GiftCatalogue/GiftSend/Search/Suggestions). Reconnect
  mem-fire bundle dashboard (rooms+suggestions+inbox+wallet).
- **UI**: Place::ALL sembilan tab (Home/Chat/Rooms/Space/Friends/Alerts/Search/Wallet/
  Settings) dengan refresh per-place; home.rs (dashboard dari state place lain),
  rooms.rs, space.rs (merge inbox+ledger, dedupe by key), alerts.rs, search.rs
  (people/rooms/chats), wallet.rs (fact card, XP bar, badges, gift picker window).
- model.rs: RoomRow/AlertRow/LedgerRow/LeaderRow/GiftRow/PersonRow/Progression/
  ActivityRow + ledger_credit/spaced_words. Context bertambah open_place.
- 33 test desktop (light palette test di-update ke kanonik). clippy & fmt bersih.

**CI/CD**: job web di ci.yml menambah `make build-web` — build statis Next.js penuh
kini diverifikasi di GitHub Actions sebelum merge; VPS tetap ringan (build berat
semua di Actions). Release v0.8.0 lewat release.yml: image GHCR migo-web+migo-migod,
binary server, APK Android, tarball web+desktop.

Verifikasi lokal (ringan): tsc bersih, 141+235 test TS, 33 test desktop, clippy 0
warning, fmt bersih, 7 gate statis (brief/infra/pydeps/protocol/entity/vector/
kotlin) hijau, build statis web sukses (8 route, /chat 184kB First Load). Compile
Android diverifikasi android.yml di CI.

## 25. Messenger-first redesign + Create Room, Leave Room, full room fitur (v0.8.1)

**ui-design.md diganti spec baru** — melarang pola yang v0.8.0 buat: sidebar SaaS besar
permanen, dashboard-first, area kosong. Diterapkan:

**Web — messenger shell:**

- **AppShell baru**: rail ikon 56px di SEMUA lebar (bukan 232px berlabel) dengan trio
  FRIENDS|CHATS|ROOMS sebagai tablist tersendiri di atas divider, label muncul sebagai pill
  overlay saat hover. Bottom bar mobile: Friends Chats Rooms Space More (trio langsung
  tersedia, sesuai §26). Sesi dibuka di **Chats**, bukan dashboard. Panel Home dihapus.
- **Friends = contact list**: seksi Friends paling atas dengan presence dot di avatar
  (usePresenceOf: seed dari profile + subscribe topic user + event live) + custom status
  sebagai baris kedua. Baris kompak, bukan kartu.
- **Create Room**: dialog (Name, Slug auto-suggest dari nama, Public/Managed segmented,
  Topic) → rooms.create → noteRoom + noteConversation → thread terbuka. Tombol "New room"
  di header panel Rooms. Validasi slug [a-z0-9-].
- **Leave Room (Android)**: ConversationRow + ChatState membawa roomId; tombol Leave di
  header chat room → rooms.leave + hapus percakapan.
- **Search dalam percakapan**: ikon search di thread header → filter pesan ter-load
  client-side.
- **Virtualized rendering**: MessageList merender window 150 pesan terakhir; "Load earlier"
  sekaligus melebar-kan window satu langkah.
- **Perbaikan bug UI v0.8.0**: grid mobile rusak saat thread terbuka (bottom-nav
  grid-row salah → keluar viewport); baris bottom bar 52px memotong konten ~60px (kini
  auto); ikon menu/coins/wallet salah gambar (menu = hamburger, coins tanpa clipPath
  hantu, wallet clasp stroke); call-overlay z-60 kalah dari modal (kini --z-overlay 300,
  modal --z-modal 400, lightbox --z-overlay, game-menu --z-dropdown); safe-area-inset-top
  di mobile header (notch/PWA standalone).

**Desktop — messenger workspace:**

- **Rail ikon 56px** (bukan 210px berlabel): brand, trio Friends|Chats|Rooms di grup
  tersendiri, divider, lalu Space/Alerts/Search/Wallet/Settings, foot: connection dot +
  theme + avatar akun (tooltip). Place::Home dihapus — sesi dibuka di Chat. Top bar hanya
  untuk layar auth. Ikon place digambar painter (bukan icon font): 9 glyph stroke 1.75.
- **Create Room**: Command::CreateRoom + RoomCreate wire (opcode 85, reply
  RoomJoinResponse) → join path yang sama. Form window di rooms.rs dengan slug-suggest.
- **Leave Room**: Command::LeaveRoom + pending_leave (ack tak membawa room id) →
  Event::RoomLeft → rooms.joined map dihapus + conversation list re-read. Baris room
  joined: Open + Leave (bukan Join).
- **Search dalam percakapan** & virtualized window ikut diterapkan di sisi chat web;
  desktop chat sudah page-bounded.

**Android:**

- Sesi dibuka di **Chats** (bukan Home; Home tetap compact hub per §15). Bottom bar
  bertambah glyph Canvas-drawn (BarGlyph: home/chats/rooms/space/more, stroke 1.75 —
  tanpa icon font). More sheet berjudul.
- **Create Room**: RoomsDomain.create (mirror join) + dialog di RoomsScreen (slug
  auto-suggest, FilterChip Public/Managed) → noteRoom → chat terbuka.
- **Leave Room**: tombol Leave di ChatScreen header (roomId via ChatState).

**CI/CD**: semua build berat tetap di GitHub Actions (build-web di ci.yml sejak v0.8.0);
rilis v0.8.1 = tag → release.yml (APK, migod binary, image GHCR, tarball web/desktop).

Verifikasi lokal (ringan, VPS): tsc bersih, 235 test web, 33 test desktop, clippy 0
warning, fmt bersih, 7 gate statis hijau, build statis web sukses (185 kB First Load
/chat).

## 26. Gaya visual main-ui.jpg: biru-violet, rail bulat, bubble solid (v0.8.2)

**Referensi baru** (`main-ui.jpg`) dipelajari dan diterapkan ke semua client — palet dan
bentuk komponen, dengan arsitektur messenger shell v0.8.1 yang dipertahankan:

**Tokens kanonik** (v2.0.0): accent **#4c6ef5** (dark) / **#3b5bdb** (light); permukaan dark
charcoal ala referensi (`#141519` bg, `#1b1c24` panel, `#24252e` sunken); presence hijau
`#2fce7e`/`#0ca678`; bubble-out **accent solid** (gradient dibuang); radius bahasa baru:
bubble 16px dengan ekor sudut, composer kapsul, kontrol rail bulat.

**Web**: rail 56px dengan tombol **bulat 44px** — aktif = **lingkaran accent terisi** berupa
ikon putih; `.rail-user` bulat; ikon header `.icon-btn` ghost bulat; baris percakapan rounded
12px tanpa divider, **presence dot peer** di avatar (usePresenceOf: seed profil + subscribe
topik + event live); tombol search di header Chats (→ section Search); bubble masuk `panel-alt`
radius `4px 16px 16px 16px` (ekor kiri-atas), bubble keluar accent solid radius
`16px 4px 16px 16px` (ekor kanan-atas); composer textarea **kapsul 999px** bg sunken,
attach ghost, **kirim bulat accent solid**; themeColor PWA `#141519`.

**Desktop**: theme.rs palet referensi (DARK: surface `#141519`/`#1b1c24`/`#24252e`, accent
`#4c6ef5`, on-accent putih; LIGHT: `#f1f2f6`/putih, accent `#3b5bdb`) + test tema di-update;
`rail_button` aktif = **lingkaran accent terisi 40px** (bukan rect + bar); `place_icon` ikon
aktif memakai contrast ink; `widgets::send_button` baru — lingkaran accent dengan paper plane
Canvas; composer input kapsul (stroke radius 20).

**Android**: Theme.kt skema referensi (AccentLight `#3b5bdb` / AccentDark `#4c6ef5`, surface
charcoal, secondary presence hijau, outline/variant baru, MigoExtra faint/gold); monogram
re-tint; glyph kirim composer diganti **paper plane Canvas** (dari teks ">").

**iOS native dibatalkan** (keputusan pengguna) — iOS tetap dilayani web PWA yang dirilis ini.

**Deployment**: web client di VPS di-build ulang (static export) dan `serve.mjs` di-restart
pada port 19992 — verifikasi HTTP 200 untuk /, /chat/, /design/, /login/, healthz; CSS live
membawa accent `#4c6ef5` tanpa sisa cyan. Build berat tetap di GitHub Actions.

Verifikasi lokal: tsc bersih, 235 test web, 33 test desktop, clippy 0 warning, fmt bersih,
gate statis hijau, build statis sukses.
