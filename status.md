# Status Implementasi Migo

Dokumen ini adalah turunan yang mudah dibaca dari **migo.md section 177 (IMPLEMENTATION
STATUS)**. migo.md tetap satu-satunya sumber kebenaran; kalau keduanya berbeda, migo.md yang
benar dan file ini yang salah. Gate `python3 tools/scripts/brief-audit.py` menegakkan section
177 secara mekanis: ia menolak crate yang ditandai BUILT tanpa test, crate yang punya test tapi
masih ditandai belum, dan crate yang muncul di dua blok sekaligus.

Terakhir diselaraskan: 27 Agustus 2026, pada commit yang memindahkan `migo-moderation` dan
`migo-notify` ke BUILT.

## Ringkasan

| Kategori                                                       | Jumlah                           |
| -------------------------------------------------------------- | -------------------------------- |
| Selesai: kode, test, clippy bersih                             | 18 crate Cargo + 9 komponen lain |
| Kode lengkap, test belum ditulis (workspace Cargo)             | 6 crate                          |
| Kode lengkap, test belum ditulis (di luar workspace Cargo)     | 5 komponen                       |
| Kode lengkap, kompilasi diverifikasi di CI, test belum ditulis | 1 komponen                       |
| Belum ada kode sama sekali                                     | 1 komponen                       |
| Sudah di schema dan codegen, handler belum ditulis             | 3 item                           |
| Baru ada di dokumen                                            | 16 item                          |

## 1. Selesai: kode lengkap, ada test, clippy bersih

Sebuah item hanya boleh berada di sini bila `cargo build`, `cargo clippy --all-targets` tanpa
satu pun peringatan, `cargo doc` tanpa intra-doc link rusak, dan `cargo test` semuanya hijau.

| Komponen                   | Isi singkat                                                       | Test       |
| -------------------------- | ----------------------------------------------------------------- | ---------- |
| `migo-core`                | id, timestamp, error, config, metrics, random, secret             | 66         |
| `migo-wire`                | codec frame: varint, zigzag, MSE, flag, limit                     | 91         |
| `migo-protocol`            | hasil codegen IDL: opcode, error code, feature bit, fault         | 27         |
| `migo-crypto`              | Ed25519, X25519, X3DH, double ratchet, sender key, AEAD, KDF, MAC | 129        |
| `migo-store`               | 14 trait domain, backend SeaORM dan backend in-memory             | 8          |
| `migo-cache`               | 6 trait cache, backend in-memory dan Redis dengan Lua atomik      | 48         |
| `migo-ratelimit`           | token bucket berbasis cost di atas 7 surface section 120          | 34         |
| `migo-auth`                | registrasi, sign in, access token 130 byte, rotasi refresh        | 67         |
| `migo-messaging`           | kirim, edit, hapus, reaksi, receipt, riwayat, envelope E2E        | 39         |
| `migo-presence`            | presence per device di cache, TTL tiga kali heartbeat             | 26         |
| `migo-economy`             | listing, wallet, statement, purchase, transfer, mata uang in-app  | 12         |
| `migo-keys`                | publish dan bundles: identity key, signed prekey, one-time prekey | 34         |
| `migo-rooms`               | 15 metode Roomkeeper: pembuatan, join, roster, peran, moderasi    | 108        |
| `migo-social`              | 19 metode Graph: pertemanan, follow, block, favourite, privasi    | 111        |
| `migo-media`               | 8 metode Library: begin, status, commit, abort, fetch_url, delete | 50         |
| `migo-moderation`          | 7 metode Warden: laporan, queue, keputusan, aksi, audit, skor     | 84         |
| `migo-notify`              | 8 metode Notifier: notify, inbox, badge, token push, sweep        | 63         |
| `packages/protocol`        | paket TypeScript hasil generate dari IDL yang sama                | 11         |
| `packages/wire`            | codec frame TypeScript, pasangan dari `migo-wire`                 | 16         |
| `packages/crypto`          | primitif kripto web di atas paket `@noble`                        | 21         |
| `tools/protocol-codegen`   | generator Rust dan TypeScript dari IDL                            | dipakai CI |
| `tools/entity-codegen`     | generator entity SeaORM dari schema                               | dipakai CI |
| `shared/protocol/schema`   | IDL itu sendiri: 29 opcode, error code, feature bit               | gate       |
| `shared/protocol/vectors`  | vector konformans wire dan kripto                                 | 21 + 48    |
| `tools/vectors`            | pembangkit dan pemverifikasi vector                               | dipakai CI |
| `.github/workflows/ci.yml` | seluruh build, lint, test, dan rilis binary                       | jalan      |

Angka pada kolom Test adalah angka yang dinyatakan migo.md section 177 bila ada. Untuk lima
crate teratas yang tidak menyebut angka, yang ditulis adalah jumlah atribut `#[test]` dan
`#[tokio::test]` di disk. Keduanya tidak selalu sama, karena satu atribut dapat menjalankan
banyak case: contract suite `migo-cache` misalnya dijalankan terhadap dua backend sekaligus.

## 2. Kode lengkap, test belum ditulis (workspace Cargo)

Keenam crate ini sudah lengkap kodenya dan lulus `cargo build` serta `cargo clippy
--all-targets` tanpa peringatan, tetapi belum punya test. Inilah pekerjaan yang sedang berjalan
sekarang, dalam urutan ini:

| Urutan | Crate             | Isi singkat                                                         | Keadaan        |
| ------ | ----------------- | ------------------------------------------------------------------- | -------------- |
| 1      | `migo-games`      | 6 metode Referee: katalog, mulai, main, selesai, papan skor         | sedang ditulis |
| 2      | `migo-bots`       | 7 metode Bots: register, authenticate, rotate_token, izin           | belum          |
| 3      | `migo-federation` | 17 metode Mesh: peer, status, sinkronisasi, transport antar node    | belum          |
| 4      | `migo-gateway`    | transport realtime: mesin state koneksi, frame, backpressure        | belum          |
| 5      | `migo-api`        | permukaan REST/JSON layer 4 yang diizinkan section 118              | belum          |
| 6      | `migod`           | composition root layer 5, satu-satunya crate yang menyusun semuanya | belum          |

## 3. Kode lengkap di luar workspace Cargo, test belum ditulis

| Komponen          | Isi singkat                                                    |
| ----------------- | -------------------------------------------------------------- |
| `packages/sdk`    | SDK TypeScript di atas `packages/wire` dan `packages/protocol` |
| `clients/web`     | web client full client side, dilayani di port 19991            |
| `clients/desktop` | native desktop client Rust dengan UI modern                    |
| `tools/loadgen`   | pembangkit beban untuk gateway dan API                         |
| `infra`           | compose, migrasi, dan berkas penyebaran                        |

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

Tahap test bukan pekerjaan tulis ulang. Sejauh ini ia menemukan sembilan cacat nyata pada kode yang
sudah dianggap selesai, dan semuanya diperbaiki pada commit yang sama dengan test yang
menemukannya:

| Crate             | Cacat                                                                                   | Perbaikan                                                  |
| ----------------- | --------------------------------------------------------------------------------------- | ---------------------------------------------------------- |
| `migo-social`     | `pending` melaporkan permintaan yang belum dijawab sebagai sudah disetujui              | membaca kolom keadaan yang benar                           |
| `migo-social`     | `block` menghapus edge tanpa menghitungnya, sehingga hitungan relasi melenceng          | penghapusan ikut mengurangi hitungan                       |
| `migo-media`      | tidak ada pemeriksaan identitas sama sekali di seluruh crate                            | `require_identity` sebelum pemungutan biaya di 7 metode    |
| `migo-media`      | lebar, tinggi, dan durasi diperiksa di `begin` lalu dibuang sebelum ditulis             | format tiket naik ke versi dua dan membawa ketiganya       |
| `migo-media`      | `commit` yang diulang ditolak sebagai objek yang sudah ada                              | dijawab dari baris yang ada tanpa menyentuh penghitung     |
| `migo-moderation` | `file_report` menerima caller yang membawa akun tanpa device                            | identitas akun dan device diperiksa sebelum biaya dipungut |
| `migo-store`      | `open_reports` in-memory mengurut menurut urutan tulis, PostgreSQL menurut `created_at` | double diurutkan menurut `created_at` lalu `report_id`     |
| `migo-notify`     | lima metode yang menghadap client tidak memeriksa identitas pemanggil                   | `require_identity` sebelum pemungutan biaya                |
| `migo-cache`      | `CacheKey::new` menolak underscore, sehingga scope coalescing panic di build debug      | assertion menerima underscore, titik dua tetap dilarang    |

## 9. Aturan yang mengikat status ini

Diambil dari migo.md section 177, karena aturannya sendiri adalah bagian dari statusnya:

1. Status WAJIB diperbarui pada commit yang sama dengan perubahan kodenya.
2. Sebuah item hanya boleh ditandai selesai bila punya test yang benar-benar dijalankan CI.
3. Ketiga blok yang namanya memuat TEST BELUM DITULIS WAJIB kosong pada saat rilis, dan sebuah
   item hanya boleh berpindah keluar dari blok itu menuju selesai, tidak pernah sebaliknya.
