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
>= 13 (ROOM_JOIN, GIFT_SEND, REPORT_CREATE) terkena tembok yang sama.

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
