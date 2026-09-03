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

## 27. Login tanpa captcha + integrasi rate limit (v0.8.3)

**Captcha** — keputusan produk: sign-in tidak pernah di-gate captcha; register tetap
threshold-gated (3 kegagalan per IP), recovery tetap wajib proof:

- `migo-auth/service.rs`: `sign_in` tidak lagi memanggil `enforce_captcha`; counter
  kegagalan tetap dicatat (`note_captcha_failure`) sehingga register mengeras terhadap
  IP yang salah password berulang. Test baru
  `a_sign_in_never_requires_a_captcha_even_once_the_gate_is_engaged` memfinalisasi
  kontrak (login benar tanpa proof = 200 + grant, saat register tanpa proof pada gate
  yang sama = `CAPTCHA_REQUIRED`).
- `/v1/config` kini membawa `captcha.enabled` (Policy.captcha_enabled), sehingga client
  dapat menyembunyikan UI captcha tanpa menebak dari penolakan.
- **Web**: halaman login tanpa widget captcha sama sekali (widget hanya di register).
- **Desktop**: seksi captcha dilepas dari form sign-in; hanya form register yang
  mem-fetch challenge.
- **Android**: `RATE_LIMITED`/`CAPTCHA_REQUIRED`/`INVALID_CAPTCHA` kini diterjemahkan
  `readable()` menjadi kalimat yang bisa ditindaklanjuti ("Too many requests. Wait N
  seconds…"), bukan gagal diam-diam.

**RATE_LIMITED** — sisi client dari kontrak 429, yang sebelumnya tidak pernah
dieksekusi:

- SDK `Rpc.call`: bila server menjawab read dengan `RATE_LIMITED` + `retry_after_ms`,
  client menunggu (dibatasi 10 detik) lalu mengulang **sekali**. Hanya opcode baca
  (conversation list, sync, room list/roster, relationship, profile, inbox, search,
  suggestions, seluruh economy) yang di-retry; mutation (send/gift/friend) tidak —
  retry otomatis atas mutasi adalah cara sebuah pesan terkirim dua kali.
- `friendlyError` (web): prefix `SYMBOL: ` dari pesan SDK kini di-strip — "RATE_LIMITED:
  Too many requests. Retry in 5 s" sampai ke pengguna sebagai "Too many requests. Retry
  in 5 s".
- **Batch presence**: `MigoClient.watchUsers(ids)` — SATU frame SUBSCRIBE untuk N topik
  user (server sudah menerima daftar; desktop sudah memakai pola ini). `usePresenceOf`
  web memakainya, memangkas burst terbesar sesi baru dari satu frame per teman menjadi
  satu frame per daftar.
- **Deployment VPS**: `migod` di-build ulang dan direstart dengan bucket dinaikkan 2x
  (`MIGO_RATE_LIMIT__USER_BURST=400`, `USER_REFILL_PER_SECOND=100`,
  `ANONYMOUS_BURST=40`, `ANONYMOUS_REFILL_PER_SECOND=10`).

**Verifikasi live di VPS**: 4x login salah menyalakan gate → login benar TANPA captcha
= `200` + token; register tanpa proof saat gate panas = `400 CAPTCHA_REQUIRED`;
`/v1/config` membawa `captcha.enabled: true`. Web client di-build ulang dan serve.mjs
direstart (semua rute 200; halaman login tanpa referensi captcha).

Lokal: 1600+ test Rust hijau (termasuk auth-flow 6/6), fmt/clippy bersih, 235 test
web, fmt-check-js hijau.

## 28. Lockout bertingkat untuk kegagalan login (v0.8.4)

**Ladder** — lima password salah untuk satu identifier mengunci percobaan berikutnya
selama satu menit; setiap tiga kegagalan berikutnya menaikkan dua menit (1, 3, 5, 7, …)
sampai cap 24 jam. Percobaan selama lockout ditolak cepat tanpa cek password dan tanpa
menghitung; login sukses mereset seluruh catatan.

- **Protokol**: error baru `1406 AUTH_LOCKED` (HTTP 429) di `errors.json` — codegen
  memancarkan Rust/TS/Kotlin; table http_status menghasilkan 429 untuk REST.
- **Server**: `migo-auth/src/lockout.rs` — `LockoutGate`, gate murni state+Timestamp
  (waktu dari `context.now`, tanpa clock dependency), kunci = identifier terlipat
  (keputusan produk: username global, lintas IP; trade-off DoS akun diterima dan
  didokumentasikan). Kabel ke `sign_in`: cek di awal (sebelum percobaan dibebankan),
  `record_failure` di kedua jalur gagal (unknown-user & bad-password), `record_success`
  saat sukses. Config `auth.lockout`: `MIGO_AUTH__LOCKOUT__{ENABLED, INITIAL_FAILURES,
STEP_FAILURES, BASE_SECONDS, STEP_SECONDS, MAX_SECONDS}`.
- **Client**: Android `readable()` menambah cabang AUTH_LOCKED ("Account temporarily
  locked. Try again in N seconds."); web login menampilkan kalimat server verbatim
  (friendlyError sudah strip symbol) + test baru; desktop menampilkan server message.

**Verifikasi**: unit lockout (tangga 5→60s, +3→180s→300s, ceiling, reset, refuse cepat,
kunci independen); integrasi auth-flow `repeated_sign_in_failures_lock_the_account_on_a_
climbing_ladder` (5 salah → ke-6 = 429 AUTH_LOCKED + retry_after_ms; clock maju → login
sukses; ladder reset); **live di VPS**: 5 salah → percobaan ke-6 dengan password BENAR =
`429 AUTH_LOCKED, Retry in 34 s`; setelah lockout lewat login benar = 200 + token; ladder
ter-reset (4 salah berikutnya = plain 401). 1600+ test Rust, 236 test web, clippy/fmt
bersih, gate statis hijau.

## 29. Port stabil di IP publik 152.53.102.150:8080 (v0.8.5)

**Masalah**: port server & default client sering berganti — desktop `localhost:18080`
(split gateway `18081`), Android loopback `18080`, web fresh visit menebak dari origin
dan SDK menormalkan non-loopback ke `Wss/Https` + `+1` port. Hasil: first-run selalu
salah dan user harus mengetik manual.

**Web**: `NEXT_PUBLIC_MIGO_API_URL=http://152.53.102.150:8080` di-bake ke `out/` lewat
`.env.production.local` (gitignored). Fresh visit, snapshot, dan derive gateway semua
stabil: `serverEndpointFromUrl` kini dinormalisasi ke **WS/Http, same-port** untuk URL
env (gateway tidak lagi `+1`, skema mengikuti REST), matching `migod` single-listener
`0.0.0.0:8080`.

**Desktop**: `Settings::default_for_dev()` → `default_production_server_endpoint()`
baru: `152.53.102.150:8080`, plain `Http/Ws`, `gateway_port = 8080`, `Transport::WebSocket`.

**Android**: `ServerEndpoint.publicDeploymentDefault():152.53.102.150:8080` (plain,
same-port) dan `AppSettings.serverEndpoint` default-nya. Fresh install langsung ke
live server; edit di form tetap persist dan menang di resume path.

**VPS stabil**: `migod` tetap `0.0.0.0:8080`, CORS `http://152.53.102.150:19992` +
`localhost`; `.migod.env` (gitignored, `chmod 600`) menyimpan fixed env + template
`server/migod.env.example.vps`; build tidak lagi menebak port.

**Verifikasi live**: `curl /health → {"status":"ok"}` via `127.0.0.1` & `152.53.102.150`;
`GET /` `GET /chat/` `GET /login/` semua `200`; JS chunk di `out/` mengandung
`152.53.102.150:8080`.

## 30. Redesign seluruh client ke new-client-ui.tsx: tab strip, banner oranye, teal-and-orange (v0.9.0)

**Referensi**: `docs/design/new-client-ui.tsx` — identitas teal/oranye, tab strip di atas
(Friends/Chats/Rooms/Games/Feed + tab chat dinamis yang bisa ditutup), banner profil oranye
dengan avatar dropdown, login gradient cyan. Model tab yang sama untuk PC dan mobile.

**Token v3.0.0** (`shared/design/tokens.json`): light = cream `#fdfbf7` + teal `#00838f`;
dark = deep-teal `#0c1517` + cyan `#00bcd4`; gradient banner `#ea580c→#f97316→#f59e0b` dan
login `#0093af→#00acc1→#00838f` theme-independent; `shape.tabChip = 12`. Dicerminkan ke CSS
variable web, `theme.rs` desktop, dan `Theme.kt`/`MigoExtra` Android (plus token nav/banner/login).

**Web**: `TabStrip` + `ProfileBanner` + `AvatarMenu` + `GamesPanel` menggantikan rail ikon dan
bottom-nav; tab chat dinamis (`#c=<id>` tetap sumber kebenaran, tombol X per tab); login/register
restyle gradient cyan + kartu glass + tombol oranye (logika captcha/lockout tidak diubah).

**Desktop (egui)**: `Place` baru (Friends/Chats/Rooms/Games/Feed + panel), tab strip 46px +
banner gradient 58px via `Mesh` colored-vertex, menu avatar (Profile/Credits & TopUp/Alerts/
Search/Logout), tab chat MDI (`open_chats`/`active_chat`), `games.rs` katalog statis jujur,
auth full-viewport gradient cyan.

**Android (Compose)**: bottom bar + More sheet diganti `TabStrip` (chip aktif accent-bright +
underline oranye, chip chat dengan ✕, badge unread) + `ProfileBanner` (dropdown: My Profile,
My Credits & TopUp, Alerts, Search, Exit/Logout; pill saldo $MIG nyata); `Section` baru
(HOME dihapus — wallet read saat sign-in mengisi saldo banner); `GamesScreen` baru;
`SignInScreen` gradient cyan + kartu glass + tombol oranye; `BarGlyph` → `TabGlyph`
(FRIENDS/CHATS/ROOMS/GAMES/FEED).

**Verifikasi**: 235+ test web hijau, cargo fmt/clippy `-D warnings`/test desktop (34) hijau,
`make kotlin-check` hijau, gates statis hijau; build APK + release penuh via GitHub Actions
(`ci.yml`, `android.yml`, `release.yml` tag v0.9.0).

## 31. Login "something went wrong": server sekarang durable (Postgres), endpoint basi di-heal (v0.9.1)

**Diagnosis**: REST login di `:8080` sehat (register/login/WS round-trip lewat SDK OK);
masalahnya dua. (1) `migod` jalan dengan `MIGO_STORE__BACKEND=memory` dan tanpa
`node.signing_key`, jadi restart pukul 11:21 menghapus semua akun — log menunjukkan lima
sign-in gagal + lockout dua menit kemudian. (2) Klien menyimpan `ServerEndpoint` lama
(port/scheme pra-v0.8.5); REST ke port yang sudah tidak dilayani gagal sebagai `TypeError`
bukan-SDK → web menampilkan fallback generik "Something went wrong. Please try again."
(Android: envelope kosong → "Something went wrong. Try again.").

**Server**: container `migo-postgres` (postgres:17-alpine, volume named, `127.0.0.1:15433`)
sekarang backend store — akun bertahan restart (diverifikasi: register → restart migod →
login sukses); `MIGO_NODE__SIGNING_KEY` (32 byte) dipasang sehingga token juga bertahan
restart. `.migod.env` diperbarui (tetap gitignored, `chmod 600`).

**Web**: `loadServerEndpoint` sekarang juga meng-heal port REST basi saat snapshot
menyebut host yang sama dengan alamat deployment yang di-bake build (host lain = milik
self-hoster, tidak disentuh); halaman login jatuh ke `defaultServerEndpoint()` saat belum
ada snapshot (dulu tombol Sign-in mentah selamanya).

**Android + desktop**: record tersimpan yang menyebut host deployment dengan port/scheme
lama dinormalisasi ke `publicDeploymentDefault()` / `default_production_server_endpoint()`
(web juga melewati migrasi legacy `server_url`); pesan error tanpa envelope kini menyebut
status HTTP ("The server answered with an error (HTTP …)") alih-alih kalimat generik.

**Verifikasi**: 240 test web hijau, cargo fmt/clippy `-D warnings`/test desktop (36) hijau,
`make kotlin-check` hijau; live: register/login/WS via SDK ke `152.53.102.150:8080` sukses,
`out/` di-build ulang dan served di `:19992`.

**Follow-up (masih v0.9.1)**: `ServerEndpoint.fromRestUrl` — jembatan resume sesi di
Android — membuang scheme URL aslinya dan menebak TLS untuk host non-loopback, jadi
resume sesi yang tersimpan dari `http://152.53.102.150:8080` mencoba `https://…:8080`,
gagal di socket, dan pengguna dilempar kembali ke layar sign-in setiap restart app.
Sekarang scheme URL adalah ground truth: `https://` → pasangan TLS (gateway = port),
`http://` → pasangan plain (gateway = port+1 hanya untuk loopback/dev; host publik =
single-port seperti deployment ini). Dua test baru + satu round-trip test untuk origin
deployment; `make kotlin-check` hijau.

## 32. TCP/WebSocket default, QUIC opsi kedua yang sungguhan (v0.10.0)

**Spec (migo.md)**: arah transport dibalik dan dinyatakan eksplisit — WebSocket di atas
TCP adalah transport realtime **default**; QUIC adalah **opsi kedua**, dan server hanya
mengiklankan bit fitur `QUIC` bila listener QUIC diaktifkan lewat konfigurasi. Federation
juga diluruskan: TLS 1.3 di atas TCP (mesh yang sudah BUILT) sebagai default, QUIC/TLS 1.3
sebagai opsi kedua — teks lama yang menulis QUIC sebagai jalur utama federation
bertentangan dengan mesh raw-TCP + FED_HELLO Ed25519 yang sesungguhnya ada.

**Server**: `migod::quic` — listener QUIC opsional (quinn 0.11 + rustls TLS 1.3,
self-signed leaf via rcgen, identitas tetap dibuktikan di lapisan aplikasi lewat
AUTHENTICATE, postur yang sama dengan mesh). Satu stream dua-arah = satu sesi realtime;
framing stream section 138 (prefix panjang u32 big-endian + frame) diimplementasikan
sebagai `QuicStreamTransport` di atas `Transport` trait, cancel-safe persis tuntutan
trait (byte parsial mendarat di buffer milik transport, bukan di future yang di-drop).
Prefix musuh (> `MAX_FRAME_BYTES`) ditolak sebelum byte kedua dialokasikan. Aktifasi:
`MIGO_QUIC__BIND` kosong (default) = tidak ada listener, bit QUIC tidak diiklankan; diisi
= listener di-bind **dan** bit QUIC di-OR-kan ke set fitur node pada saat yang sama di
composition root — kegagalan bind membatalkan startup, jadi set yang diiklankan tidak
pernah bohong tentang set yang dilayani.

**Client**: di ketiga client (web/desktop/Android) QUIC sekarang pilihan kedua yang bisa
dipilih dan dipersistenkan apa adanya — bukan lagi radio disabled / "coming soon" /
downgrade diam-diam. Copy jujur: build ini belum punya runtime QUIC di JS/Kotlin, jadi
jalur kabelnya masih WebSocket (Android memutuskan di `MigoSession.wireGatewayUrl` pada
saat connect; record tersimpan tidak diubah). Desktop combo: "QUIC (second option)".
Pairing transport/scheme divalidasi simetris di ketiga platform (WS↔WS/WSS,
QUIC↔QUIC/QUIC-TLS); pembaca Settings Android men-snap scheme ke family transportnya
supaya record parsial tidak bisa crash read path.

**Verifikasi**: workspace server penuh — fmt, `cargo clippy --workspace --all-targets
-- -D warnings`, `cargo test --workspace` (termasuk 5 unit test framing + 3 test
integrasi `tests/quic_listener.rs` yang mengikat listener sungguhan dan menggerakkan
klien quinn/rustls nyata) semua hijau; desktop fmt/clippy/test (36); TS build + eslint +
test web (240) + test SDK (141); `make kotlin-check` (14 selftest / 0 masalah);
`make brief-check` (41 pemeriksaan, bersih). Rilis lewat GitHub Actions seperti biasa:
tag `v0.10.0` → release.yml (binary migod, tarball web, binary desktop, APK debug,
2 image GHCR).

**Follow-up (masih v0.10.0)**: listener QUIC dinyalakan di deployment produksi
(`MIGO_QUIC__BIND=0.0.0.0:18443` di `.migod.env`, restart bersih; TCP/WebSocket
di `:8080` tetap default). Test live baru `a_live_listener_answers_hello_with_a_
welcome_that_carries_the_quic_bit` (ignored, env-gated `MIGO_QUIC_LIVE_ADDR`)
menjalankan handshake penuh ke node sungguhan: TLS 1.3 → satu stream dua-arah →
HELLO dengan bit QUIC diminta → WELCOME dengan bit QUIC ter-negosiasi
(features = irisan, jadi klien yang tidak meminta tetap tidak menerimanya —
kontrak, bukan cacat). `cargo fmt`/`clippy -D warnings`/suite in-process hijau;
test live hijau terhadap `152.53.102.150:18443`.

## 33. Pilihan transport QUIC dinaikkan ke permukaan sign-in (v0.10.1)

Transport picker sebelumnya hidup di dalam disclosure "Server" yang collapse
secara default — ada, tapi tidak terlihat sampai dibuka. Sekarang di web
(segmented control WebSocket | QUIC di bawah toggle, selalu terlihat) dan
Android (radio transport langsung di bawah tombol Server), satu tap langsung
meng-commit swap transport pada endpoint tersimpan dengan pasangan scheme yang
benar (QUIC→QUIC/QUIC-TLS, WebSocket→WS/WSS via aturan loopback) — tanpa perlu
buka panel dan konfirmasi ulang host/port. Catatan jujur QUIC ("butuh server
dengan listener QUIC aktif; build ini masih terhubung via WebSocket") ikut
tampil selama QUIC dipilih, dan ringkasan server kini menyebut transport
aktif. Select transport duplikat di dalam panel web dihapus (satu sumber
kebenaran); detail host/port/scheme tetap di dalam panel. `make kotlin-check`
hijau; web build/test/lint/Prettier hijau; bundle baru di-deploy ke :19992.

**Follow-up (v0.10.2)**: desktop kini sama dengan web dan Android. Disclosure
"Server" di egui menampilkan pemilih transport WebSocket | QUIC yang selalu
terlihat di bawah toggle (`server_form::show` kini menerima endpoint ter-commit
sebagai `value`, cermin prop `value` di web); satu klik menukar transport pada
endpoint tersimpan lewat `swap_transport` — transport dan pasangan schemenya
saja yang berubah (QUIC→QUIC/QUIC-TLS, WebSocket→WS/WSS via aturan loopback),
host/port/gateway port ikut apa adanya — lalu hasilnya dikembalikan sebagai
`Some(endpoint)` sehingga `AuthState::apply_server` yang sudah ada yang
mem-persist dan me-re-seed draft. ComboBox transport lama di dalam panel
dihapus (satu sumber kebenaran); catatan jujur QUIC kini tampil di luar panel
selama transport ter-commit adalah QUIC, dan baris ringkasan menyebut transport
aktif. Tiga test baru untuk `swap_transport` (host publik → QUIC-TLS/HTTPS,
loopback → QUIC/HTTP, balik ke WebSocket → pasangan host). Desktop
fmt/clippy `-D warnings`/test (39) hijau.

## 34. Desktop benar-benar terhubung via QUIC (v0.10.3)

Sebelumnya memilih QUIC di desktop hanya mengubah label — wire-nya tetap
WebSocket. Sekarang pilihan itu menunjuk wire sungguhan: `net/quic.rs`
(quinn 0.11 + rustls TLS 1.3, versi sama persis dengan listener server)
menghubungi endpoint terpilih — satu koneksi QUIC, satu stream dua-arah,
satu sesi, framing length-prefix u32 BE per migo.md section 138. HELLO
menambahkan bit QUIC (negosiasi = irisan, jadi bit harus diminta), WELCOME
yang balik menentukan jalannya: bit QUIC ada → sesi hidup di QUIC
(`Realtime::Quic`); bit tidak ada atau listener tak terjangkau → fallback
bersih ke WebSocket default dengan status jujur
`Connection::Fallback("Encrypted · WebSocket")` — server yang bekerja bukan
layar error. Certificate verifier menerima leaf self-signed server dengan
sengaja (sesi diautentikasi token di HELLO, postur yang sama dengan mesh
federation); keep-alive 15 detik menjaga NAT tetap hangat; pembaca stream
cancel-safe dengan buffer internal. Status pill di banner/chat/settings
mengenali varian Fallback (hijau-teal "Connected", label pill menyebut
transport). Catatan QUIC di form server diperbarui agar jujur: "jika server
tidak menawarkannya, klien ini jatuh ke WebSocket dan mengatakannya." Test
live baru `the_client_transport_completes_a_live_quic_handshake`
(env-gated `MIGO_QUIC_LIVE_ADDR`) menjalankan panggilan yang sama dengan
worker — handshake TLS 1.3 → stream → HELLO+bit QUIC → WELCOME dengan bit
QUIC → round-trip PING terjawab — dan hijau terhadap produksi
`152.53.102.150:18443`. Desktop fmt/clippy `-D warnings`/test (44) hijau.

## 35. Transport TCP native: satu warisan mig33v46, tiga jalur (v0.11.0)

Desain transport-bindings baru (migo.md section 138) membagi jalurnya dengan
jelas: client native (desktop, Android) bicara raw TCP dengan framing biner
length-prefix u32 big-endian — warisan mig33v46 — web client tetap WebSocket
karena browser tidak punya TCP socket API, dan QUIC menjadi opsi kedua untuk
semua; federasi antar-node tetap TLS 1.3 over TCP. Bit fitur TCP_TRANSPORT
(21) hanya di-advertise selama listener benar-benar terikat, dan negosiasi
tetap irisan — klien yang tidak meminta tidak menerimanya. Di server,
`MIGO_TCP__BIND` membuka listener TCP yang membawa surface realtime penuh:
satu koneksi = satu sesi, frame dibatasi MAX_FRAME_BYTES yang dicek _sebelum_
body hostile di-buffer, dan pembacaan cancel-safe dengan buffer milik
transport. End-to-end test menjalankan kontrak fitur penuh (HELLO → WELCOME
dengan bit TCP → round-trip) plus prefiks hostile yang harus ditolak. Desktop
dan Android memakai TCP sebagai transport native default dengan fallback
jujur ke WebSocket — label status selalu menyebut transport yang benar-benar
dipakai. CI server/desktop hijau; build Android hijau di Actions.

## 36. Satu akun, satu root 32-byte: identitas ML-DSA-65, dompet EVM, kontainer .migo (v0.12.0)

**Desain** (migo.md section 134-137): seluruh akun diturunkan dari satu rahasia
root 32-byte yang digenerate di device dan tidak pernah meninggalkannya. Setiap
kegunaan adalah satu ekspansi HKDF-SHA-256 sendiri dengan label ber-versi —
`MIGO/IDENTITY/V1` (login ML-DSA-65 / FIPS 204), `MIGO/EVM/V1` (BIP-32/BIP-44
coin 60), `MIGO/E2EE/V1` (seed identity X3DH device pendiri), `MIGO/BACKUP/V1`
(jadwal kunci kontainer); kredensial per-device sengaja TIDAK diturunkan dari
root agar root yang bocor sendirian tidak bisa menyamar jadi device terdaftar.
Server tidak menyimpan material privat sama sekali — hanya public key.

**migo-account**: crate referensi Rust dengan vektor lintas-bahasa. Modul
root/identity/evm/container digenangi test, dan vektor konformansi
(`shared/protocol/vectors/crypto/account-{domains,evm,mldsa,container}.json`)
dipakai Rust, Python, TypeScript, dan Kotlin untuk memastikan empat platform
menurunkan byte yang sama. Port TypeScript (`packages/crypto/src/account/`,
@noble/post-quant + @noble/scure/bip32 + hash-wasm) dan port Android
(`:core`, lazysodium untuk primitif dasar + BouncyCastle untuk ML-DSA-65,
Keccak-256 dan aritmetika secp256k1 BIP-32) lulus vektor yang sama;
SDK web mendapat 11 metode REST identitas (`identityLoginChallenge`,
`addDeviceChallenge`, `identityLogin`, `addDevice`, `rotationChallenge`,
`rotateIdentity`, `publishIdentityKey`, `devices`, `wallets`,
`registerWallet`, `archiveWallet`) yang mirror `account.rs`.

**Server** (migrasi 0004: tabel identity_keys, login_challenge, wallet, plus
kolom status/public_credential di device): login tanpa password jadi upacara
challenge — klien meminta challenge
Login (identifier di-resolve lewat email/telepon/username, device harus milik
akun dan aktif), menandatangani payload dengan kunci identitas (context
`migo-auth-login-v1`) dan kunci device (`migo-auth-device-v1`), lalu menerima
Grant. Challenge palsu menyamarkan akun yang tidak ada (anti-enumeration).
AddDevice memakai `account_id` dari kontainer .migo, menegakkan limit device
saat issue, dan membuat baris Pending; rotasi identitas jalan di context
terpisah `migo-auth-rotate-v1`. Dompet EVM diregistrasi dengan alamat + index
derivasi, bisa diarsip. Semua upacara mencatat security event untuk audit.

**Desktop**: device pendiri menyimpan root + identity E2EE deterministik +
kredensial acak; device tambahan (sign-in password) TANPA root; device hasil
restore (impor kontainer) mendapat root kembali. Vault MIGOVLT1 bertambah
field opsional FIELD_ROOT/FIELD_DEVICE_CREDENTIAL dan tetap membuka vault
lama. Refresh token gagal di device pemegang root otomatis jatuh ke login
upacara ML-DSA (sign payload apa adanya, tanpa re-encode). Backup .migo dibuat
hanya dari device pemegang root (UI jujur menyebutnya), restore lewat form
sendiri; registrasi mendaftarkan dompet index 0 otomatis dan device
menyinkronkan dompet 0..7 ke server; panel Settings menampilkan daftar device
(dengan tanda kredensial/current) dan dompet dengan tombol Archive.

**Keamanan & CI**: gerbang CI baru memindai diff terhadap pola rahasia
(brief section 183) agar token/kunci tidak pernah ter-commit. Pintu akhir:
server 1750 test hijau, desktop 56 test hijau, web/TS 29 test vektor akun
hijau, lint/fmt/clippy bersih di seluruh workspace.

## 37. Dompet AVAX: tanda tangan EIP-1559 di device, Avalanche C-Chain sebagai chain pertama (v0.13.0)

**Desain** (migo.md section 184, ADR-0014): dompet EVM pertama Migo adalah
native AVAX di Avalanche C-Chain — transfer native saja, transaksi type 0x02
(EIP-1559) saja, jaringan dipilih lewat nama (Mainnet / Fuji) dan tidak
pernah lewat URL, RPC di-pin (43114/43113), AVAX 18 desimal dengan fee
dikutip di nAVAX (9 desimal). Mainnet tampil default dengan acknowledgement
"uang sungguhan, tidak bisa dibatalkan" sebelum tombol kirim terbuka. Kunci
privat tidak pernah diekspor dalam plaintext. Aturan sesi (spec 44): RPC
pertama adalah eth_chainId dan harus cocok; broadcast re-verifikasi — hash
asing ditolak.

**Signing** (migo-account, diikuti TypeScript dan Kotlin dengan vektor yang
sama): EIP-1559 signing hash = Keccak-256(0x02 || RLP(sembilan field)),
signature ECDSA-secp256k1 low-s, dan EIP-712 untuk typed-data. Tidak ada
RPC ke blockchain di dalam crate crypto — yang keluar dari device hanya
transaksi mentah yang sudah ditandatangani. Vektor konformansi
`account-tx.json` memakai integer string desimal agar empat bahasa membaca
angka yang sama tanpa presisi float.

**ChainClient** (SDK TS + Kotlin :core): JSON-RPC publik ke network yang
di-pin, bukan lewat server Migo — server tidak pernah jadi proxy blockchain.
getBalance/getNonce/estimateGas/getFees untuk membangun transaksi,
broadcast mengembalikan acceptance dan tidak lebih, dan `track` mengikuti
hash sampai akhir yang jujur (spec 41): CONFIRMED hanya dari receipt
status 1, REVERTED dari status 0, DROPPED bila hilang melewati toleransi,
EXPIRED saat deadline; backoff ×1.5 dengan batas. Penerimaan bukan
konfirmasi — UI menyebutnya BROADCAST.

**Tiga klien, satu alur**: layar konfirmasi menampilkan SEMUA field yang
akan ditandatangani (From/To/Amount/Max fee/Max priority fee/Gas
limit/Nonce/Chain) sebelum tombol konfirmasi aktif; kegagalan parse
ditolak sebelum RPC pertama keluar; `from` yang bukan wallet 0 device ini
ditolak, bukan ditandatangani. Desktop (egui) menambahkan panel AVAX +
alur kirim + toast settle; Android (Compose) menambahkan panel AVAX di
WalletScreen dengan FilterChip dua jaringan, form kirim satu layar, dan
baris aktivitas; web menambahkan seksi AVAX di WalletPanel dengan
chip/network, form kirim, dan daftar aktivitas — semuanya membaca wallet 0
dari root (device tanpa root mendapat satu kalimat jujur: buka dompet di
device yang memegang backup akun).

**SDK web — root di key store**: registrasi web sekarang device pendiri
(mints root, identitas E2EE diturunkan dari domain E2EE root) dan snapshot
key store membawa root + daftar transaksi terlacak; device tambahan tetap
tanpa root. Enrolment material publik (kunci identitas ML-DSA + alamat
wallet 0) berjalan idempotent di setiap register/resume — pintu upgrade
legacy yang sama dengan klien native. Record transaksi ditulis saat
broadcast (reload di tengah tracking kehilangan akhirannya, bukan fakta
bahwa value sudah keluar) dan dimutasi di array hidup yang disegel pada
persist berikutnya.

**Pintu akhir**: vektor akun lulus di Rust/TS/Kotlin/Python, SDK 154 test
hijau, web 248 test hijau (termasuk aritmetika AVAX/nAVAX dan penolakan
parser), server dan desktop hijau di CI, Android hijau di Actions, lint/
fmt/clippy bersih. Google Drive backup ditunda sesuai keputusan fase.

## 38. Setiap keperluan di sheet-nya sendiri: kartu auth yang lega (v0.13.1)

**Desain**: kartu login/registrasi kembali lega — hanya brand, sub-kalimat,
field identitas/password, captcha, tombol, dan tautan silang. Pemilihan
server pindah ke sheet tersendiri (BottomSheet: bottom sheet di ponsel,
dialog kecil terpusat di ≥768px) yang dibuka dari satu tautan kecil di
pojok kanan bawah dalam kartu (`Server · host:port · Transport`), sehingga
keputusan server tidak lagi memakan ruang kartu. Prinsip yang sama
diterapkan ke semua keperluan sekunder: pemilih penerima hadiah, form
kirim AVAX (Build → Confirm dua langkah di satu sheet, judul sheet
mengikuti langkah), dan ganti password di Settings — masing-masing
keperluan hidup di sheetnya sendiri, bukan menumpuk di panel.

**ServerForm sebagai body sheet**: transport tetap satu-satunya pilihan
yang bukan draft — segmented WebSocket/QUIC commit seketika; host/port/
scheme tetap draft sampai "Use this server", yang sekaligus menutup sheet.
Commit dari sheet adalah satu-satunya jalan perubahan endpoint di halaman.
CSS `.server-disclosure*` diganti `.server-form*`; body picker di dalam
sheet melepas frame-nya sendiri (border/shadow/padding) supaya tidak
kotak-di-dalam-kotak.

## 39. Register yang lega + service worker yang tahu diri (v0.13.2)

**Kenapa masih ada yang melihat UI lama**: server sudah menyajikan bundle v0.13.1
(HTML no-cache, CSS content-hashed), tetapi service worker memakai nama cache
tetap `migo-web-v1` — byte sw.js tidak pernah berubah antar rilis, jadi cache
lama menumpuk dan browser tidak wajib mengambil worker baru. Sekarang build
menstempel nama cache dengan SHA commit (`tools/stamp-sw.mjs`, postbuild):
setiap rilis mengubah byte sw.js → browser update worker → activate
menghapus semua cache bernama lain → tidak ada shell/chunk lama yang selamat.

**Layout register**: brand jadi wordmark compact satu baris (bukan hero
bertumpuk yang memakan 60px), sub-kalimat dipendekkan, urutan field
diatur identitas-dulu (username → email opsional → password), password
menampilkan aturan minimal 10 karakter (dan minLength) sesuai config
server, username/email dapat placeholder dan bebas spellcheck. Background
gradient tetap fixed dan kartu scroll di layar pendek (warisan v0.13.1).

## 40. File akun .migo: ditawarkan setelah register, dipulihkan saat login (v0.13.3)

**Setelah register**: kartu registrasi menawarkan sheet "Save your account"
— akun otomatis tersimpan di IndexedDB browser (record username +
account id + snapshot key store), dan pengguna ditawari mengunduh file
akun `migo-<username>.migo`: kontainer terenkripsi Argon2id (64 MiB,
3 pass) + XChaCha20-Poly1305 dari `@migo/crypto`, dilindungi recovery
credential pilihan pengguna (minimal 8 byte, bukan password akun,
§182). Credential dinilai lokal sebelum hashing dijalankan; redirect ke
chat menunggu penawaran ini selesai karena root hanya lengkap di momen
ini. Perangkat tanpa root mendapat satu kalimat jujur, tanpa tombol mati.

**Saat login**: browser yang pernah masuk menampilkan chip
"Continue as {username}" — tinggal password; "Use a different account"
membuka form lengkap. Browser yang belum mengenal akun mendapat tautan
"Restore from account file" (kiri bawah kartu, server tetap kanan):
pilih file .migo + recovery credential → `openContainer` → root →
`KeyStore.founding` mereproduksi identitas founding device secara
deterministik (seeds E2EE diturunkan dari domain root yang sama) →
sign in dengan username + password seperti biasa, dan sesi berjalan
sebagai perangkat founding (root ada, riwayat E2EE terbaca), bukan
perangkat tambahan. Kegagalan open selalu satu kalimat (§182): salah
credential, file berubah, dan file asing tidak dibedakan.

**Provider**: login menerima `restored?: KeyStore` opsional; record
akun ditulis pada register dan login sukses, dan sengaja tidak dihapus
saat logout — logout mengakhiri sesi, bukan hubungan browser dengan
akun.

## 41. Register: captcha bersebelahan dengan field di layar PC (v0.13.4)

Kartu register kini melebar (660px) di atas breakpoint sheet dan membagi
diri menjadi dua kolom: username, email, dan password di kiri, captcha
di kanan — seorang pengguna PC membaca form dan menyelesaikan challenge
dalam satu lebar pandangan, bukan men-scroll kartu yang lebih tinggi.
Kolom field sedikit lebih lebar (1.15fr) karena input adalah pekerjaan
utama; gambar challenge tetap 200px. Di ponsel grid runtuh menjadi
susunan biasa — field lalu captcha — persis seperti sebelumnya, dan
kartu login tidak tersentuh (tetap 420px; form-nya tidak punya captcha).

## 42. Client desktop untuk Windows: build native MSVC di CI dan release (v0.13.4)

Rilis kini menghasilkan `client_desktop-<versi>-x86_64-pc-windows-msvc.zip`
bersama tarball Linux-nya — pengguna Windows membongkar .zip, konvensi
platformnya sendiri, bukan .tar.gz yang hanya dibuka dengan berat hati.
Buildnya native di runner windows-latest (MSVC), bukan cross-compile, dan
jauh lebih pendek dari job Linux-nya: eframe bersinar di OpenGL yang
sudah dibawa Windows, jadi tidak ada padanan daftar apt-get, dan
permukaan spesifik-platform klien memang kecil — permission file vault
sudah punya fallback `#[cfg(not(unix))]`, path data lewat crate `dirs`
yang me-resolve ke %APPDATA%. Pengemasan memakai Git Bash (sha256sum
tersedia) dan bsdtar `-a` yang menyimpulkan format zip dari ekstensi.

CI juga menambah job `desktop-windows` (`cargo check --locked`) di setiap
push: Windows hijau di antara rilis, bukan baru ketahuan rusak saat tag.
Check saja, bukan clippy — clippy sudah berjalan di Linux, dan job ini
hanya menjawab satu pertanyaan: apakah klien masih dikompilasi untuk host
Windows? Aset bertambah dua (zip + sidecar), release menjadi 10 aset.

## 43. Model UI baru new-ui-02: dua panel independen di semua client (v0.13.5)

Mockup referensi `new-ui-02.tsx` mengganti strip tab global + satu body
dengan split dua panel yang independen. **Panel kiri** (~32% di PC,
seluruh layar di ponsel) memiliki strip tealnya sendiri (Friends, Chats,
Rooms, Games, Feed — tab Chats tetap dipertahankan karena daftar
percakapan adalah permukaan messenger) di atas banner profil oranye.
**Panel kanan** berjalan dengan state-nya sendiri: tab menu-nya (Feed,
Games, Alerts, Search, TopUp, Profile, Settings) saat tidak ada
percakapan aktif, atau bar chat slate-800 dengan chip percakapan yang
bisa ditutup plus tombol "‹ Menu Panel" saat ada. Klik di kiri tidak
pernah mengganggu kanan — itulah tawaran model ini. Di bawah breakpoint
PC kedua panel bergantian: kiri adalah aplikasi, chat/panel menu
menutupinya dengan tombol kembali di barnya masing-masing.

- **Web**: `AppShell` dirombak jadi dua kolom (`.app-left`/`.app-right`,
  grid 32%/flex di ≥1024px, satu kolom bergantian di bawahnya); komponen
  baru `ChatTabBar` (back + chevron scroll + chip) dan `PanelTabBar`
  (judul "Panel: X" + tombol kecil); mesin tab di `chat/layout.tsx`
  memegang `leftTab`/`rightTab`/`chatTabs`/`activeChat` terpisah, dengan
  fragment URL tetap satu sumber kebenaran percakapan terbuka — "‹ Menu
  Panel" menyembunyikan thread tanpa menutupnya.
- **Android**: chat dan panel menu (Alerts, Search, Wallet, Profile)
  kini MENUTUPI shell dengan barnya sendiri (`PanelBar` baru, "‹ Menu
  Panel"), bukan menjadi tab di strip; strip kiri kehilangan chip chat.
  `stripSection` di `AppState` mengingat tab kiri saat panel menutupi,
  sehingga back kembali ke tab yang tadi dilihat.
- **Desktop**: egui `Panel::left("left-pane")` 32% (300–540px) berisi
  strip + banner + konten tab sistem; `CentralPanel` jadi panel kanan
  dengan `chat_bar` (slate, back + chip) atau `panel_bar` (teal,
  "✦ Panel: X" + tombol Feed/Games/Alerts/Search/TopUp/Settings).
  `Place::RIGHT_TABS` + `right_label()` (Wallet→"TopUp") menggantikan
  chip panel yang bisa ditutup; `select_place` merutekan tab sistem ke
  kiri dan panel ke kanan.

## 44. Fase 7 §16-§18: daftar device dan pencabutannya di semua client (v0.13.5)

Manajemen device akun (migo-update-1.md §16-§18) turun ke semua
permukaan. Server mendapat `POST /v1/devices/{device_id}/revoke` yang
memanggil `revoke_device` service (menutup semua sesi device itu, lalu
menandai baris device-nya `revoked`) dan menjawab `{ok, revoked}` —
`revoked` adalah jumlah sesi yang diakhiri. Sebuah cacat produksi ikut
tertutup: `POST /v1/auth/sessions/{session_id}/revoke` selama ini
salah sambung — ia meneruskan _session id_ ke `revoke_device`
(pencarian device, selalu 404), sehingga tombol revoke per-baris di web
tidak pernah bisa bekerja; kini ia memanggil `sign_out` yang memang
dimaksudkan untuk satu sesi. Test rute baru
(`revoking_one_session_and_then_its_device_over_the_routes`) menjalankan
kedua jalur: daftar sesi, cabut satu sesi (token-nya 401, yang lain
tetap hidup), _login_ ulang yang mengklaim device pertama sehingga
device itu punya sesi hidup lagi, lalu cabut device-nya dan buktikan
token ketiga mati dan statusnya "revoked".

- **SDK**: `devices()` dan `revokeDevice({device_id})` di MigoClient,
  di atas `GET /v1/devices` dan route revoke baru di rest.ts.
- **Web**: panel Settings terbagi dua — "Devices" (akar akun:
  termasuk yang revoked, tanda "This device"/"Revoked"/"holds a
  sign-in credential", tombol Remove dengan `window.confirm` yang
  menyebut nama device, §70) dan "Sessions" seperti sebelumnya.
- **Desktop**: baris device di settings dengan ghost button Remove
  (tooltip menjelaskan konsekuensinya); hanya device yang bukan
  current dan belum revoked yang menawarkannya.
- **Android**: section "Devices" di ProfileScreen — `DevicesState` di
  AppState, `loadDevices()`/`revokeDevice()` di AppViewModel (lazy-load
  saat pertama masuk Profile), AlertDialog konfirmasi yang menyebut
  nama device sebelum server bertindak.
- **Alat**: `tools/chatbot` menambah `room-smoke.ts` — uji asap satu
  node: register dua akun, _login_ ulang alice di client baru, buat
  room publik, bob join, lalu chat round-trip yang diverifikasi tiba
  dan terdekripsi di kedua sisi. Ia menemukan (dan dokumentasikan
  lewat penggunaannya) bahwa origin loopback `http://` berarti kebijakan
  split-port dev (gateway = port+1): server VPS satu-port harus
  dituju lewat URL publiknya.

## 45. Registrasi idempoten: percobaan ulang melipat ke akun yang sama (v0.14.0)

Migo-update-1.md §12 menuntut register yang aman dicoba ulang: jaringan
putus setelah server menulis akun, klien mencoba lagi, dan upaya kedua
harus menemukan akun pertama alih-alih membuatkannya duplikat. Server
(migo-auth) menandai percobaan register dengan idempotency key; percobaan
ulang dengan key yang sama mengembalikan akun yang sudah dibuat beserta
token barunya, bukan error "username taken" — register hanya gagal
bila key-nya berbeda. Klien (web/desktop/Android) menyimpan _founding
root_ akun sebelum percobaan pertama selesai, sehingga percakapan
idempoten lintas percobaan; key akar bertahan melewati kegagalan jaringan
sekalipun. Juga pada rilis ini: tab Chats keluar dari strip kiri di
semua client (f495fff — keputusan yang dibalik di v0.14.4, lihat #47).

## 46. Tata letak web dirapikan: sudut ponsel, kontrol akun, banner (v0.14.1–v0.14.3)

Tiga perbaikan kecil-kecil rapat setelah IA dua-panel. v0.14.1: kartu
login dan bar tab panel tidak lagi terpotong di sudut layar ponsel —
viewport fit. v0.14.2: kontrol akun (sandi, ganti nama, perangkat)
memimpin panel friends, tepat di bawah banner, sebelum daftar — bukan
terkubur di bawah. v0.14.3: kontrol banner memeluk kartunya; flex-basis
dan padded gap dihapus sehingga menu akun dan toggle tema duduk rapat di
kartu oranye, bukan melayang di lajur kosong.

## 47. Panel kanan: satu bar tab, Feed dulu, semua sisanya bisa ditutup (v0.14.4)

Model "menu panel" resmi pensiun. Panel kanan kini punya satu bar tab
saja: chip pertama adalah **Feed** — isi istirahat pane, selalu ada,
tidak pernah bisa ditutup — disusul satu chip yang bisa ditutup per
hal terbuka: percakapan, arcade games, atau panel (satu chip per jenis).
`RightPaneState { tabs, active }` satu objek; menutup chip jatuh ke
berikutnya, menutup yang terakhir menyisakan Feed — itulah fallback
yang dipertimbangkan pane kosong. Fragment `#c=<id>` tetap satu sumber
kebenaran percakapan terbuka. Di bawah 1024px tombol kembali menjadi
ikon-saja yang tersembunyi di PC. Tab Chats juga pulang ke strip kiri
(membalik f495fff) bersama titik oranye saat ada yang belum dibaca —
inilah yang membuat pesan masuk terlihat lagi: daftar percakapan
(`ConversationList`) sempat tidak punya rumah di IA baru, sehingga pesan
yang tiba di penerima tidak punya permukaan sama sekali; protokolnya
sendiri terbukti baik lewat uji round-trip dua akun. Test shell kini
255 dan meng-assert lima tab sistem, titik belum-dibaca, dan tidak ada
lagi "Menu Panel".

## 48. Kapasitas room dari pertemanan: 5 kursi dasar, +10 per teman, plafon 33/50 (v0.14.5)

Kapasitas room bukan lagi knob deployment melainkan aturan produk:
`capacity_for(kind, friends) = min(plafon, 5 + 10 × teman)`. Akun yang
tidak dikenal siapa pun membuka room maksimal 5 kursi — ruang
pertemuan orang asing adalah bentuk spam, dan akun yang belum
divouch siapa pun tidak diberi aula besar di hari pertama. Setiap
pertemanan yang diterima menambah 10 kursi, karena teman adalah orang
nyata yang menjawab permintaan nyata — sinyal termurah yang sudah
dibawa store untuk "akun ini dikenal orang yang bisa
dimintai tanggung jawab". Permintaan pertemanan yang masih pending
tidak menghasilkan apa-apa. Plafon per jenis: **33 untuk room publik,
50 untuk managed** (managed bisa dibaca server — postur moderasi yang
menjustifikasi plafon lebih besar). `max_members` eksplisit di atas
jatah ditolak VALIDATION_FAILED dengan pesan yang menyebut jatahnya;
tanpa `max_members`, room lahir tepat sebesar jatah pembuatnya. Mates
jenuh dihapus (`i64` saturating) dan hitungan teman via
`count_relationships(Friend)`. `RoomsConfig` kehilangan
`default_max_members`/`max_members_ceiling` — hanya `home_region`
yang tersisa, karena plafon adalah aturan produk yang sama di semua
deployment. Tiga test baru: room orang asing kecil, kapasitas tumbuh
10 kursi per teman (pending tidak dihitung), dan plafon jenis membatasi
(publik 33, managed 50) pada pembuat berlima teman.

## 49. Admin global untuk room publik: halaman CRUD khusus Owner/CEO (v0.14.6)

Migo-update-1.md #48: admin global untuk room publik, dipilih oleh
Owner/CEO Migo. Dua keputusan bentuknya. Pertama, **siapa Owner/CEO
dinamai konfigurasi, bukan diturunkan dari data** —
`MIGO_AUTH__OWNER_ACCOUNT_ID` (TOML bentuk teks 26 karakter Id;
`serde(default)`, `None` = permukaan tertutup untuk semua orang,
termasuk operator yang lupa mengisinya). Kedua, **tabel `global_admin`
(migrasi 0005) berdiri sendiri**: PK adalah account itu sendiri —
grant adalah kehadiran akun di tabel, revoke adalah hilangnya, tidak
ada grant kedua yang perlu dibedakan dari yang pertama; `granted_by`
mencatat siapa yang menunjuk, riwayat grant maupun revoke keduanya
masuk `audit_entry` (actor Operator, action `global_admin.grant` /
`global_admin.revoke`).

Permukaannya di `Authenticator` (mengikuti preseden devices/wallets):
`admin_standing` (jawaban "bolehkah saya buka?" — selalu sukses, owner
salah/benar bukan error), `global_admins`, `grant_global_admin`
(by username, idempoten — grant ulang mempertahankan grant pertama),
`revoke_global_admin` (menghapus akun yang bukan admin = diam `Ok`,
aturan bentuk yang sama dengan archive wallet). Semua tulisan dan
daftar hanya lewat `require_owner` — admin global tidak bisa
mengangkat admin lain. REST: `GET /v1/admins/whoami`, `GET /v1/admins`,
`PUT /v1/admins` (body username), `DELETE /v1/admins/{account_id}`
(204).

Penegakan di room: admin global mensanksi anggota non-owner di room
**publik** tanpa menjadi anggota — `require_public_over` menjaga dua
pemeriksaan require_over (id wajib, tidak boleh menembak diri sendiri)
dan dua perlindungan owner ("owner bukan pangkat": tidak tersentuh
siapa pun, termasuk admin global). Yang gugur adalah keanggotaan, bit
izin, dan perbandingan pangkat. Room **managed** tetap kebal —
taman berdinding satu owner, deployment tidak memoderasikannya lewat
proksi. Designasi juga bukan imunitas: admin global yang tergabung
sebagai anggota biasa tetap bisa dimoderasi ownernya.

Web: entri menu **Global Admins** (ikon shield) di menu avatar banner
hanya muncul setelah `adminStanding()` menjawab owner — permukaan
tersembunyi bagi semua orang lain, dan panel menolak dengan pesan jujur
bila ditipu dibuka. Panelnya: formulir angkat per-username (tombol
terkunci sampai ada nama), daftar admin saat ini, dan Revoke per baris
dengan konfirmasi. SDK: `adminStanding` / `globalAdmins` /
`grantGlobalAdmin` / `revokeGlobalAdmin`. Test: 6 di migo-auth
(tanpa owner = tertutup, grant+revoke, hanya owner, idempoten,
akun harus ada, jejak audit), 6 di migo-rooms (sanksi tanpa keanggotaan,
owner terlindungi, managed kebal, bukan diri sendiri, stranger ditolak,
designasi bukan imunitas), 4 penyajian panel web.

## 50. CORS membuka PUT dan DELETE — perbaikan appoint admin global (v0.14.7)

Laporan produksi: Owner (love) membuka halaman Global Admins, mengangkat
jono, dan mendapat "Something went wrong" — tanpa jejak apa pun di server
(tabel `global_admin` kosong, tanpa audit, tanpa request). Diagnosisnya
bukan di jalur domain: **preflight CORS**. Web di origin `:19992` memanggil
REST di `:8080` lintas origin, dan `PUT /v1/admins` adalah request
non-sederhana — browser mengirim OPTIONS dulu, lapisan CORS jawab
`Access-Control-Allow-Methods: GET, POST` saja, dan browser **memblokir
request sebelum sempat dikirim**. Fetch menolak dengan `TypeError` polos
yang berada di luar kosakata error SDK, jatuh ke fallback generik web.
`DELETE /v1/admins/{id}` (revoke) dan `PUT /v1/auth/contact` mengidap
penyakit yang sama.

Perbaikannya dua sisi. Server: `allow_methods` diperluas ke
GET/POST/PUT/DELETE dengan komentar yang menjelaskan gejala
"method hilang = kegagalan jaringan opak di route yang benar". Test
`a_preflight_for_every_surface_verb_is_granted` mengunci keempat kata
dari OPTIONS preflight asal origin terdaftar. SDK: semua helper REST
(`#post`/`#put`/`#delete`/`#get`) kini lewat `#exchange` yang melipat
reject fetch (TypeError browser untuk koneksi ditolak, putus jaringan,
preflight CORS yang tidak diberi) menjadi `TransportError` — sehingga
web menyampaikan "Could not reach the Migo server", bukan pesan kosong.
Test `rest-transport.test.ts`: TypeError → TransportError, reject tanpa
pesan tetap bernama, verdict server tetap `RemoteError` murni.

## 51. SUGGESTIONS dengan graf kosong: daftar kosong, bukan error (v0.14.8)

Laporan produksi: "tambah fitur cari teman dan tambahkan ke daftar
pertemanan". Fiturnya ternyata sudah ada lengkap — server (`search`
prefix username + contains display name, case-folded, privacy-filtered),
web (panel Friends: kolom cari + tombol Add friend + Requests
Accept/Decline), Android (FriendsScreen + SearchScreen), desktop (Search
place, PEOPLE + Add). Probe SDK live membuktikan alur
search → request → accept → edge pertemanan dua arah bekerja di produksi.

Yang rusak bukan fiturnya, tapi **pintu masuknya**: probe UI headless
menemukan panel Friends gagal total dengan error mentah `user_ids` dan
spinner abadi. Akarnya di `handle_suggestions`: handler mengumpulkan id
dari hasil `suggest()` lalu menyerahkannya ke `profiles()` — padahal
`profiles()` menolak batch kosong (`field_required("user_ids")`) dan
akun baru (atau siapa pun yang grafnya belum punya saran) menghasilkan
nol saran. Satu RPC SUGGESTIONS yang gagal itu menjatuhkan seluruh
`reload()` panel web (Promise.all) — jadi kolom pencarian, daftar
teman, dan tombol Add friend tidak pernah sempat dirender. Klien
Android dan desktop yang memuat saran di startup kena penyakit yang
sama.

Perbaikan: handler menjawab daftar kosong saat tidak ada saran — graf
kosong adalah keadaan wajar akun baru, bukan kegagalan baca. Test
`an_empty_graph_suggests_nobody_and_profiles_refuses_an_empty_batch`
di spec_social mengunci dua sisi seam yang disusun handler: `suggest`
wajib menjawab Vec kosong (bukan error), `profiles` wajib menolak batch
kosong — sehingga guard di handler tidak kehilangan alasannya.

## 52. Login hanya lewat file kunci .migo — gender, captcha pergi, panel My Account (v0.14.9)

Permintaan: sistem login diperbarui menjadi **hanya** key file `.migo` yang
diunduh saat selesai pendaftaran plus passphrasenya; kolom pendaftaran
menjadi username, passphrase, email, gender; card captcha (background
putih, tombol reload, "easier challenge") dihilangkan; setelah daftar
muncul modal menawarkan unduh file kunci dan langsung masuk; di menu
avatar ada "My Account" dengan unduh/ganti file kunci, ganti passphrase
(tanpa bisa mengganti username), ganti email.

Sisi server tiga hal. (1) **Gender**: migrasi 0006 menambah kolom
`gender smallint` di `profile` (1 laki-laki, 2 perempuan, 3 lainnya, null
tidak menyatakan), diekspos sebagai `Option<i16>` tervalidasi di
`RegisterRequest` — nomor di luar penomoran ditolak VALIDATION_FAILED.
Ditest di migo-auth (tercatat di profil; diam tetap diam) dan migo-api
(201/400). (2) **Route kontak**: `PUT /v1/auth/contact` akhirnya diwujudkan
— SDK sudah memanggilnya sejak v0.14.7 dan komentar CORS sudah
menyebutnya, tapi routenya belum pernah terpasang. Test: 204 saat alamat
valid, 400 untuk bukan email/telepon, 401 tanpa bearer. (3) Tidak ada
perubahan captcha di server: gate dikendalikan `MIGO_CAPTCHA__ENABLED`,
dan di produksi `.migod.env` kini mematikannya. Upacara identitas
(§182) memang tidak pernah digerbangi captcha.

Sisi web, halaman login diganti total: pilih file `.migo`, ketik
passphrase, selesai — tidak ada lagi kolom username/password, chip akun
tersimpan, maupun sheet restore. Penyedianya (`provider.tsx`) kini
memegang `loginWithFile`: buka container (salah passphrase = satu kalimat
jujur §182, tidak bisa dibedakan dari file rusak), lalu upacara ML-DSA
dua tingkat — device record tersimpan (IndexedDB baru
`device-record-store`, seed kredensial per akun) menjawab _login
challenge_ sebagai device yang sama, dan bila belum ada / device sudah
dihapus server, jatuh ke _add-device_ yang mencetak kredensial baru dan
menyimpannya untuk login berikutnya; tanpa record, setiap login akan
melahirkan device baru dan batasnya cuma delapan. Sesi berjalan sebagai
device founding (root dari file mereproduksi identitas founding), grant
disimpan, materi akun (kunci identitas + wallet 0) dipublikasikan
idempoten.

Pendaftaran memakai satu passphrase untuk dua peran: password di server
dan kunci penyegelan file — makanya modal pasca-daftar tidak meminta
kredensial kedua: langsung "Download key file" (disegel dengan passphrase
tadi) dan "Continue". Widget captcha dihapus dari komponen, CSS
(`.captcha-*`, `.register-grid`, `.auth-account-chip`,
`.auth-restore-link`), dan testnya.

Panel **My Account** baru (entry pertama menu avatar, ikon shield):
identitas read-only (@username + MGO-XXXXXXXX, "Your username can never
be changed."), email write-only (updateContact; tidak ada API untuk
membaca email kembali — fieldnya jujur soal itu), ganti passphrase
(changePassword + saveSession untuk grant pengganti; setelah sukses form
berhenti menawarkan submit dan justru memperingatkan file lama masih
terbuka dengan passphrase LAMA, menawarkan file segar bila device
memegang root), dan unduh file kunci (hanya device dengan root; tanpa
root, satu kalimat jujur dan tombol mati). Seksi password pindah dari
Settings ke sini.

Test: server 61 migo-auth / 73 migo-api; web 274 (wire shape upacara
login & add-device, round-trip device record dengan seed, gender di body
register, tawaran penyegelan tanpa input rahasia kedua, empat seksi
panel + keadaan tanpa root). Catatan desain: klien desktop dan Android
masih memakai login password (server tetap mendukung); pengguna web yang
sudah ada tapi tidak menyimpan file .migo harus masuk dari perangkat
yang masih bersesi atau membuat akun baru.

## 53. Sistem room penuh: kapasitas, reconnect 2 menit, mute personal, vote kick 50%, admin global, dan eskalasi ban menyeluruh (v0.15.0)

Permintaan: room menolak pendatang saat batas tercapai; member bebas
keluar-masuk; yang putus koneksi diberi 2 menit untuk nyambung lagi
sebelum dinyatakan pergi; mute bersifat personal — pesan yang di-mute
hilang di **semua** room, tapi hanya di mata pemuternya; kick lewat vote
50% dari penghuni; admin global bisa kick/ban tanpa vote; daftar room
menampilkan okupansi hidup "Jambi 2/33"; dan tiap perubahan anggota
diberitahukan ke room. Ditambah satu eskalasi: kena kick oleh admin
global lebih dari 3x = ban dari seluruh chatroom.

**Wire.** Empat opcode baru — `ROOM_VOTE_KICK` (90), `ROOM_VOTE_EVENT`
(91, coalesce per room), `ROOM_SANCTION` (92), `MUTE_SET` (120) — dan
`MemberChange` kini enumerasi (Joined/Left/Disconnected/Reconnected/
Kicked/Banned), bukan lagi sekadar flag join. Kode fault baru:
`VOTE_TARGET_IMMUNE` (1207), `NETWORK_ROOM_BANNED` (1208),
`VOTE_ALREADY_OPEN` (1511). ROOM_VOTE_KICK ditarif 5 token; membuka
vote, bersuara, dan lolosnya vote terbit di satu stream
`ROOM_VOTE_EVENT` yang di-coalesce per room.

**Store (migrasi 0007).** Tabel `room_network_ban` (satu baris per akun;
keberadaan baris = ban, unban = delete, tanpa keadaan "diangkat tapi
masih ter-row" yang bisa salah dibaca cek join). `RoomStore` bertambah:
`record_moderation_action` (jejak audit per tindakan — memori menolak
room/actor/target yang tak ada, jujur seperti FK Postgres),
`count_global_admin_kicks` (join aktor terhadap registri admin global
_saat ini_ — admin yang diberhentikan berhenti dihitung detik itu juga,
dan barisnya tetap ada sehingga re-appoint memulihkan sejarah),
`network_ban`/`set_network_ban` (upsert)/`clear_network_ban`. Tiga kasus
kontrak baru dijalankan dua backend: jejak audit FK-jujur, hitungan kick
hanya admin global aktif, dan ban jaringan satu-baris-upsert-or-delete.

**Layanan (migo-rooms).** `join` menolak saat penuh dan saat akun
ter-ban jaringan (`until` dibandingkan dengan jam crate, bukan jam
store — siapa bertindak, dia menghitung waktu). `sanction` kini
mengembalikan `Vec<Fanout>`: satu kick bisa menyapu banyak room.
Admin global berlaku di room mana pun, public maupun managed, tanpa
perlu jadi member — tapi pemilik room mutlak kebal. **Eskalasi**: kick
ke-4 oleh admin global (hitungan >3) menulis ban jaringan
(`until: None` — hanya Unban yang membalikkan) dan menyapu akun itu dari
semua room yang ia bukan pemiliknya, tiap room mendapat event Banned.
Kick terhadap member yang sudah tak aktif tidak menulis baris audit —
admin tidak bisa menggelembungkan hitungan dengan menendang kursi
kosong. **Vote kick**: satu vote terbuka per room, registry
in-memory dengan TTL 60 detik (malas: vote berikut di room itu yang
menutup yang lama); suara yang dibutuhkan `max(2, ceil(n/2))`; pemilik
room dan admin global kebal (dicek sebelum lookup target); member yang
di-mute tetap boleh bersuara — dibungkam bukan berarti dicabut
haknyanya; suara ulang idempoten; vote lolos menutup registry di dalam
lock lalu menulis `leave_room` di luar lock.

**Presence & reconnect (room_presence.rs, akar komposisi).** Online count
room adalah perpotongan "siapa member" (migo-rooms) dan "siapa
terhubung" (gateway) — dua fakta yang tidak bertemu di crate mana pun,
maka talinya dipegang composition root sebagai tally in-memory. Socket
pertama naik: tiap room kebagiani hitungan online segar, dan yang tadinya
diberi tahu offline diberi tahu `Reconnected`. Socket terakhir turun:
room diberi tahu `Disconnected`, kursi **dipertahankan**, dan timer masa
tenggang 2 menit dipasang; bila melesat saat akun masih offline dan
masih member, kepergiannya jadi nyata dengan `Left`. Pemilik room kebal
timeout — pencipta room tidak kehilangan roomnya hanya karena menutup
laptop. Pembatalan timer lewat counter generasi per akun: timer membawa
generasi saat dipasang dan diam bila sudah bergeser — tanpa handle timer
yang dilacak, tanpa lock lintas await.

**Mute personal (migo-social).** Edge Mute satu arah — dan sengaja
**tidak** membongkar apa pun: berbeda dari block yang merobohkan
pertemanan dan follow, mute adalah tombol volume, bukan vonis. Yang
di-mute tidak diberi tahu, tidak ada cara bertanya "siapa yang
me-mute saya", dan plafon 1.000 edge. Di web, `MutedProvider` memegang
set mute (server-owned, dibaca ulang tiap reset sesi) dan
`muteFilter` diterapkan pada transkrip room saja — DM satu-satu tidak
pernah difilter; membungkam kebisingan keramaian bukan memutus
seseorang, memutus itu block.

**Klien.** Daftar room web menampilkan `2/33` (online hidup / batas
maksimum, tooltip lengkap) dan mengurutkan berdasarkan online count.
Panel info room: tombol kick-vote untuk semua member dengan tally yang
naik di stream, dan tombol Mute/Kick/Ban untuk staf — UI member dan
staf, dua jalan yang wire sediakan. Notifikasi anggota ("Ana joined the
room") dirender di live region transkrip, nama diresolve belakangan,
"Someone" bila profil belum turun. Android menyamakan: RoomsScreen
okupansi, ChatScreen notice + filter mute, SDK Kotlin baru. SDK TS
menambah `voteKick`, `sanction`, `muteUser`. Captcha kembali di
pendaftaran web — inline, tanpa card putih (v0.14.9 sempat
menghilangkannya).

Test: migo-rooms 130, migo-social 115, kontrak store 52 kasus × dua
backend, gateway dan migod end-to-end, web 292.

## 54. File kunci tanpa textbox: ikon File, akun tersimpan terenkripsi, ganti akun (v0.15.0)

Permintaan: pilih file jangan lewat textbox — pakai ikon File kalau browser
belum pernah memuat file `.migo`; begitu pernah, simpan di db app **tetap
terenkripsi sesuai passphrase**; tangani semua kemungkinannya (belum
import, sudah import, mau ganti akun, dan lainnya), plus modal yang
menjelaskan semuanya dengan UI serupa register/login yang responsif di
PC/Android/iOS.

Yang disimpan browser persis seperti yang diunduh: container tersegel
utuh — header salt/nonce terbaca, root tersegel XChaCha20-Poly1305 di
dalam body. Passphrase **tidak pernah** ditulis; baris baru di
`key-file-store` (IndexedDB, satu array `key-files`) menghemat pemilih
file, bukan pintu. Identitas baris adalah salt Argon2id dari header
clear — file yang sama di-import dua kali meng-upsert (username yang
baru dipelajari saat login sukses mendarat di barisnya), salinan
re-seal dari akun sama adalah baris lain. Pendaftaran kini juga
menyimpan: sheet penawaran file kunci menyegel begitu terbuka (root
hanya di memori selama sesegar itu — menunggu tombol ditekan adalah
versi di mana sheet yang ditutup berarti file yang tak pernah ada),
menyimpannya ke store, dan tombol unduhan memakai byte yang sama tanpa
Argon2id kedua.

Layar login: input file browser disembunyikan total. Browser yang
belum mengenal file mana pun menampilkan satu tile besar ikon File;
yang sudah, menampilkan daftar akun tersimpan (terbaru dipilih otomatis,
baris terpilih membalut putih-solid seperti input card — "sedang masuk
sebagai siapa" terlihat, bukan dihafal), tombol "Use another key file"
di ujungnya, dan tiap baris bisa dilupakan (X — lupa baris terpilih
jatuh ke yang termuda sisanya). File yang baru dipilih tampil sebagai
baris "signing in with this file for the first time" dengan X untuk
membatalkan. Ganti akun = logout, dan daftar itu menunggu di layar
login. Kegagalan store bukan kegagalan login — daftar kosong adalah
tile import, upacara jalan seperti sebelum daftar ada. Sheet
penawaran didandani selevel kartu register: badge gradient cyan, lead
line yang sama jujurnya ("Your account is saved to this browser
automatically..."), tombol unduh berikon, semua di kolom terpusat yang
nyaman di satu tangan maupun sheet desktop. Ikon `file` dan `download`
baru digambar di keluarga ikon — stroke yang sama di semua platform,
bukan emoji yang berbeda wajah per OS.

Test: store (identitas salt, upsert, byte round-trip verbatim, urutan
terbaru-dulu, lupa satu baris tak menghapus tetangganya) + 296 web.

## 55. Friends sekali sebut, ikon daftar di kanan, presence naik ke banner (v0.15.0)

Layar Friends pernah menyebut namanya dua kali — judul panel, lalu
heading seksi dengan kata yang sama — dan kontrol presence milik akun
tersimpan di dalam panel itu, jauh dari identitas yang dikontrolnya.
Sekarang panel adalah daftarnya: satu judul "Friends", dan di kanannya
baris ikon horizontal — user-plus untuk Requests, block untuk Blocked,
sparkle untuk Suggestions — yang mengganti isi panel; ikon terpilih
bertanda, dan tiap ikon membawa hitungan apa yang dipegangnya
(request menunggu terlihat tanpa harus dibuka). Daftar teman sendiri
tak butuh heading di bawah judul yang sudah menamainya.

Presence dan status naik ke banner profil: dropdown keadaan duduk di
samping kiri chip coin dengan kaca yang sama, input status di bawah
baris @username — di-seed sekali dari status yang profil sudah bawa —
dan titik presence di samping nama kini mewarnai sesuai keadaan yang
dropdown pegang. Picker pecah jadi dua kontrol (PresenceSelect,
StatusInput) untuk dua kedudukan itu; keduanya tetap publish
terkontrol penuh, dan di ponsel banner tetap satu baris ambient —
input ikut baris @username yang menyembunyikan diri. Glyph baru:
user-plus dan block.

## 56. Group chat penuh: undang, mute, kick 51%, founder, ganti nama (v0.15.1)

Group pernah berhenti di pembuatan: sekali dua orang lebih masuk,
tidak ada jalan menambah, mengeluarkan, atau mengganti nama. Sekarang
satu siklus hidup utuh berdiri di atas sepuluh opcode baru (43 sampai
52), dengan tiga aliran acara langsung — member, vote, state — yang
semuanya Coalescable per percakapan kecuali member event yang tidak
boleh digabung.

Semantiknya meniru room sampai ke tempat kedua sistem itu memang
berbeda. Dua founder — pembuat dan orang pertama yang dinamainya —
adalah ingatan grup siapa yang membangunnya: mengundang adalah hak
tiap anggota, sedangkan mute, kick tanpa voting, dan ganti nama
adalah milik founder, dan founder tak tersentuh satu sama lain maupun
oleh voting (VOTE_TARGET_IMMUNE) — grup yang dibangun dua orang tidak
bisa dipotong separuh oleh salah satunya. Vote kick butuh mayoritas
ketat setengah anggota dibulatkan ke atas, satu voting terbuka per
grup, TTL 60 detik yang ditutup secara malas, dan anggota yang
dimute tetap memilih. Keluar adalah hak tanpa pintu gerbang: founder
terakhir yang pergi diam-diam menyerahkan peran pada anggota
terlama, jadi grup tak pernah kehabisan orang yang boleh mengganti
namanya. Mute ditegakkan di send (codes MUTED) tanpa fanout — roster
adalah catatannya — dan rename menaiki State event yang digabung.

Panel grup (ⓘ di header) menyatukan semuanya: undang lewat daftar
teman maupun pencarian username (dua sumber yang sama dengan dialog
percakapan baru, "In group" bagi yang sudah masuk), roster dengan
badge Founder/Member dan catatan mute yang sedang jalan, tombol
Mute 1 jam / 1 hari / 7 hari plus Unmute, Vote kick dengan tally
langsung, Kick founder dengan konfirmasi, ganti nama, dan Leave
Group. Keluar atau dikeluarkan menutup thread dan menghapus barisnya;
pergerakan anggota muncul sebagai baris ambient di transkrip
("Ana joined the group") lewat proyeksi place yang sama dengan room,
dan setiap member event memutar sender key — churn keanggotaan adalah
peristiwa kripto sebelum menjadi peristiwa UI.

Server: 46 test messaging, 55 contract postgres (plus satu bug nyata
yang ditemukannya: postgres menyemai role 0 = Unknown pada
insert_members, kini fail-closed ke Member), dan gerbang push-only
gateway menerima tiga opcode s2c baru. SDK: ConversationsDomain
memanjang dari 2 menjadi 9 metode plus tiga stream listener dengan
start/stop. Web: 313 test termasuk gerbang founder/vote/mute yang
dipinkan murni dan proyeksi member/state pada daftar percakapan.
Registri section 145 dan enum ConversationRole masuk migo.md.

## 57. Tabs chat jadi pilihan tampilan: chip kanan atau daftar Chats satu jendela (v0.15.1)

Cara pane kanan memegang chat adalah fakta tentang orangnya, bukan
tentang sesinya — jadi sekarang ia pengaturan. Settings (menu avatar)
mendapat bagian "Chats Tabs" dengan dua pilihan, dan pilihannya
bertahan di localStorage kunci `migo:chat-tabs-mode` sebagai string
polos (bukan material kunci, jadi aturan audit localStorage tak
tersentuh), gagal-baca dan gagal-tulis sama-sama jatuh ke default.

"Right tabs" (default, perilaku selama ini): setiap chat terbuka
menjadi chip yang bisa ditutup di pane kanan, dan chip Chats di strip
kiri disembunyikan — chip-nya memang daftarnya.

"Chats list": chip Chats kembali ke strip kiri, bar tab pane kanan
hilang sama sekali, dan sebuah chat terbuka sebagai satu jendela
penuh — fragment URL tetap satu-satunya kebenaran tentang chat yang
terbuka, jadi Back dan tombol close tak pernah berselisih. PaneBar
tipis menggantikan bar chip: satu label judul (span, bukan tombol —
tak ada yang bisa diganti), chevron back untuk satu kolom, dan close
yang mengembalikan pane ke Feed-nya. Panel sekunder dan arcade
mengikuti bentuk yang sama lewat satu state `panel`.

Pergantian mode mendamaikan ulang pane di tempat: chip dibuang (atau
panel tunggalnya), fragment effect mencetak ulang thread terbuka di
mode yang baru dipilih. Web: 322 test termasuk gerbang storage mode,
chip Chats yang hilang, bar pengganti, dan bagian Settings yang
menawarkan kedua mode dengan cerita mode aktifnya.
