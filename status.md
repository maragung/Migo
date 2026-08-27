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
bawah terhadap pohon yang bersih, yaitu ketujuh gate, 23 anggota workspace, dan 1553 test Rust
bersama 10 doc-test dan 251 test TypeScript yang semuanya hijau, lalu mencatat satu temuan baru
yang bukan berupa test yang gagal melainkan otorisasi yang tidak ada, di bagian 8b. Commit
sebelumnya memperbaiki job `gates` setelah gate konformans pecah di CI, menambahkan gate ketujuh
`make pydeps-check` supaya kelas kegagalan itu tidak terulang, dan memberi job advisory satu
pengecualian beralasan. Sebelum itu enam crate terakhir (`migo-games`, `migo-bots`,
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
| Test yang hijau pada commit ini                                | 1553 Rust + 10 doc-test + 251 TS  |
| Cacat terbuka yang belum diperbaiki                            | 1, lihat bagian 8                 |

Tidak ada satu pun test yang gagal, dan tidak ada satu pun `#[ignore]` di seluruh workspace.
Yang dilewati rustdoc hanyalah enam contoh dokumentasi bertanda ` ```ignore ` pada `migo-bots`,
`migo-economy`, `migo-games`, `migo-moderation`, `migo-notify`, dan `migo-social`: keenamnya
adalah cuplikan ilustrasi yang menuntut graph aplikasi yang sudah hidup, jadi perilakunya
dipakukan oleh test integrasi crate-nya masing-masing dan bukan oleh doc-test.

## 1. Selesai: kode lengkap, ada test, clippy bersih

Sebuah item hanya boleh berada di sini bila `cargo build`, `cargo clippy --all-targets` tanpa
satu pun peringatan, `cargo doc` tanpa intra-doc link rusak, dan `cargo test` semuanya hijau.

| Komponen                   | Isi singkat                                                       | Test       |
| -------------------------- | ----------------------------------------------------------------- | ---------- |
| `migo-core`                | id, timestamp, error, config, metrics, random, secret, clock      | 66         |
| `migo-wire`                | codec frame: varint, zigzag, MSE, flag, limit                     | 91         |
| `migo-protocol`            | hasil codegen IDL: opcode, error code, feature bit, fault         | 27         |
| `migo-crypto`              | Ed25519, X25519, X3DH, double ratchet, sender key, AEAD, KDF, MAC | 129        |
| `migo-store`               | 10 trait domain, backend SeaORM dan backend in-memory             | 96         |
| `migo-cache`               | 6 trait cache, backend in-memory dan Redis dengan Lua atomik      | 116        |
| `migo-ratelimit`           | token bucket berbasis cost di atas 7 surface section 120          | 33         |
| `migo-auth`                | registrasi, sign in, access token 130 byte, rotasi refresh        | 66         |
| `migo-messaging`           | kirim, edit, hapus, reaksi, receipt, riwayat, envelope E2E        | 38         |
| `migo-presence`            | presence per device di cache, TTL tiga kali heartbeat             | 26         |
| `migo-economy`             | listing, wallet, statement, purchase, transfer, mata uang in-app  | 12         |
| `migo-keys`                | publish dan bundles: identity key, signed prekey, one-time prekey | 34         |
| `migo-rooms`               | 15 metode Roomkeeper: pembuatan, join, roster, peran, moderasi    | 108        |
| `migo-social`              | 19 metode Graph: pertemanan, follow, block, favourite, privasi    | 111        |
| `migo-media`               | 8 metode Library: begin, status, commit, abort, fetch_url, delete | 50         |
| `migo-moderation`          | 7 metode Warden: laporan, queue, keputusan, aksi, audit, skor     | 84         |
| `migo-notify`              | 8 metode Notifier: notify, inbox, badge, token push, sweep        | 63         |
| `migo-games`               | 6 metode Referee: katalog, mulai, main, selesai, papan skor       | 95         |
| `migo-bots`                | 7 metode Bots: register, authenticate, rotate_token, izin         | 96         |
| `migo-federation`          | 17 metode Mesh: handshake, peer, urutan link, antrean keluar      | 71         |
| `migo-gateway`             | transport realtime: mesin state koneksi, frame, heartbeat (8b)    | 13         |
| `migo-api`                 | permukaan REST/JSON layer 4 yang diizinkan section 118            | 65         |
| `migod`                    | composition root layer 5, argumen, penolakan startup, graph       | 63         |
| `packages/protocol`        | paket TypeScript hasil generate dari IDL yang sama                | 11         |
| `packages/wire`            | codec frame TypeScript, pasangan dari `migo-wire`                 | 16         |
| `packages/crypto`          | primitif kripto web di atas paket `@noble`                        | 21         |
| `packages/sdk`             | SDK TypeScript di atas wire, protocol, dan crypto                 | 56         |
| `clients/web`              | PWA Next.js full client side, dilayani di port 19991              | 63         |
| `tools/protocol-codegen`   | generator Rust dan TypeScript dari IDL                            | dipakai CI |
| `tools/entity-codegen`     | generator entity SeaORM dari schema                               | dipakai CI |
| `tools/loadgen`            | pembangkit beban yang menggerakkan MigoClient sungguhan           | 84         |
| `shared/protocol/schema`   | IDL itu sendiri: 29 opcode, error code, feature bit               | gate       |
| `shared/protocol/vectors`  | vector konformans wire dan kripto                                 | 2 runner   |
| `tools/vectors`            | pembangkit dan pemverifikasi vector                               | dipakai CI |
| `.github/workflows/ci.yml` | seluruh build, lint, test, dan rilis binary                       | jalan      |

Satu baris di tabel itu membawa penanda, yaitu `migo-gateway` dengan `(8b)`. Ia memenuhi
keempat syarat yang tertulis di atas secara harfiah, dan justru itu masalahnya: syarat itu
ternyata tidak cukup untuk crate itu, karena sebuah invariant yang ditulis di kepala suite-nya
tidak punya test dan ternyata juga tidak punya kode. Baris itu wajib dibaca bersama bagian 8b,
dan tidak boleh dibaca sendiri.

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

Tahap test bukan pekerjaan tulis ulang. Sejauh ini ia menemukan empat belas cacat nyata pada
kode yang sudah dianggap selesai, dan semuanya diperbaiki pada commit yang sama dengan test yang
menemukannya. Baris kelima belas datang bukan dari test melainkan dari pipeline-nya sendiri, dan
tetap dicatat di sini karena ia adalah cacat pada sesuatu yang sudah dianggap selesai. Yang
keenam belas ada di bawah tabel, terpisah, karena ia satu-satunya yang belum diperbaiki.

### 8a. Sudah diperbaiki pada commit yang sama dengan penemuannya

| Crate             | Cacat                                                                                                                                                                                                                                | Perbaikan                                                                                                                                               |
| ----------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `migo-social`     | `pending` melaporkan permintaan yang belum dijawab sebagai sudah disetujui                                                                                                                                                           | membaca kolom keadaan yang benar                                                                                                                        |
| `migo-social`     | `block` menghapus edge tanpa menghitungnya, sehingga hitungan relasi melenceng                                                                                                                                                       | penghapusan ikut mengurangi hitungan                                                                                                                    |
| `migo-media`      | tidak ada pemeriksaan identitas sama sekali di seluruh crate                                                                                                                                                                         | `require_identity` sebelum pemungutan biaya di 7 metode                                                                                                 |
| `migo-media`      | lebar, tinggi, dan durasi diperiksa di `begin` lalu dibuang sebelum ditulis                                                                                                                                                          | format tiket naik ke versi dua dan membawa ketiganya                                                                                                    |
| `migo-media`      | `commit` yang diulang ditolak sebagai objek yang sudah ada                                                                                                                                                                           | dijawab dari baris yang ada tanpa menyentuh penghitung                                                                                                  |
| `migo-moderation` | `file_report` menerima caller yang membawa akun tanpa device                                                                                                                                                                         | identitas akun dan device diperiksa sebelum biaya dipungut                                                                                              |
| `migo-store`      | `open_reports` in-memory mengurut menurut urutan tulis, PostgreSQL menurut `created_at`                                                                                                                                              | double diurutkan menurut `created_at` lalu `report_id`                                                                                                  |
| `migo-notify`     | lima metode yang menghadap client tidak memeriksa identitas pemanggil                                                                                                                                                                | `require_identity` sebelum pemungutan biaya                                                                                                             |
| `migo-cache`      | `CacheKey::new` menolak underscore, sehingga scope coalescing panic di build debug                                                                                                                                                   | assertion menerima underscore, titik dua tetap dilarang                                                                                                 |
| `migo-store`      | token CAS game adalah timestamp milidetik, jadi dua langkah dalam milidetik yang sama membuatnya tidak bergerak dan langkah kedua menimpa langkah pertama tanpa pernah melihatnya                                                    | token didorong melewati nilai yang baru saja dicocokkan pada kedua backend, dan contract case memakukannya                                              |
| `migo-bots`       | pemanggil tanpa identitas dimeter terhadap akun yang disebut permintaannya, sehingga penyerang dapat menguras budget akun orang lain tanpa membayar apa pun                                                                          | identitas diperiksa dan ditolak sebelum limiter disentuh                                                                                                |
| `migo-core`       | staging dan production menerima kredensial database `migo:migo` yang terdokumentasi terbuka di compose dan CI                                                                                                                        | startup ditolak dengan menyebut field-nya tanpa menggemakan kredensialnya                                                                               |
| `clients/web`     | ketika server menahan pesan manusia, SDK melipat pesan kosong menjadi symbol mesin dan UI menampilkannya, sehingga NOT_FOUND dan PRIVACY_RESTRICTED yang sengaja dibuat identik menjadi dapat dibedakan                              | pesan server hanya ditampilkan bila benar-benar ada, selebihnya satu baris generik                                                                      |
| `tools/loadgen`   | logger menulis barisnya tanpa redaksi dan laporan menggemakan URL server yang utuh beserta userinfo-nya                                                                                                                              | setiap baris logger lewat `redact`, dan laporan melewatkan kedua URL lewat `sanitizeUrl`                                                                |
| `ci.yml`          | menyematkan interpreter Python untuk satu gate ikut menyembunyikan modul yang kebetulan sudah ada di image runner, sehingga generator vector kripto kehilangan `cryptography` dan gate konformans pecah di CI padahal hijau di lokal | kedua modul dipasang eksplisit dalam satu langkah, dan `make pydeps-check` membandingkan daftar itu dengan impor `tools/` yang sebenarnya di kedua arah |

### 8b. Ditemukan dan belum diperbaiki

Satu cacat, dan bobotnya tidak sebanding dengan jumlahnya. Ia ditemukan bukan oleh test yang
gagal melainkan oleh pembacaan crate `migo-gateway` untuk menyiapkan test atas invariant
otorisasi yang disebut kepala suite-nya, dan yang ditemukan bukan test yang kurang melainkan
otorisasi yang tidak ada.

`handle_subscribe` di `server/crates/migo-gateway/src/connection.rs` menagih rate limiter,
membaca `SubscribeRequest`, lalu memanggil `hub.subscribe` dengan daftar topic dari frame apa
adanya. `Hub::subscribe` di `src/hub.rs` hanya membandingkan jumlah langganan yang dipegang
sesi terhadap `max_subscriptions`. Trait `Dispatcher` di `src/dispatch.rs` punya tepat satu
metode, yaitu `dispatch`, dan tidak punya kait otorisasi, sehingga tidak ada satu pun crate
domain yang pernah ditanya apakah pemanggil berhak atas sebuah topic. Akibatnya, sesi mana pun
yang lolos handshake dapat menyebut `Topic` apa saja dan mulai menerima fan-out-nya:

- `TopicKind::Conversation` memberi seluruh metadata percakapan orang lain, yaitu `message_id`,
  `conversation_id`, `seq`, `sender_id`, `sender_device`, `kind`, `created_at`, `reply_to`,
  `edited_at`, penanda hapus, dan `sender_key_id`, beserta `envelope` tersegel apa adanya, plus
  tanda baca dan tanda sedang menulis serta event game pada percakapan itu.
- `TopicKind::Room` memberi event anggota dan keadaan sebuah room, dan untuk room yang memang
  tidak mengklaim enkripsi ujung ke ujung berarti isinya juga.
- `TopicKind::User` memberi transisi presence sebuah akun tanpa melihat blokir maupun setelan
  `show_last_seen`, yang berarti section 180 dilanggar melalui pintu ini meskipun jalur baca
  presence sendiri menghormatinya. Yang tidak bocor hanyalah Invisible, karena
  `visible_state` sudah memproyeksikannya menjadi Offline sebelum penyiaran.

Ada cacat kedua yang menempel pada yang pertama. Sebuah frame hanya dibatasi
`MAX_FRAME_BYTES`, sementara satu `Topic` hanya berbiaya sekitar 18 byte di wire, jadi satu
frame dapat menyebut puluhan ribu topic. Hari ini itu baru soal biaya di `Hub`; begitu
otorisasi benar-benar menanyai crate domain, itu menjadi pengali beban terhadap database lewat
satu frame yang dibayar satu tagihan rate limiter.

Perbaikannya sudah dirancang dan setengah ditulis, lalu disimpan di `git stash` ketika arah
kerja dialihkan ke dokumen ini, jadi ia belum ada di pohon dan belum ada di commit mana pun.
Bentuknya: satu metode batch `authorize_topics` pada trait `Dispatcher` yang sudah ada, bukan
port ketiga, sehingga pernyataan migo.md bahwa gateway bicara ke domain lewat tepat dua trait
tetap benar; default trait yang menolak segalanya sehingga implementor yang lupa gagal tertutup
dan bukan terbuka; pemotongan daftar topic pada `MAX_SUBSCRIPTIONS` sebelum domain ditanya,
yang menutup pengali di atas; penolakan yang dituang ke daftar `rejected` yang sudah ada dan
tetap tanpa alasan, sehingga bukan anggota tidak dapat dibedakan dari tidak ada, sesuai section
48; serta `AppDispatcher` di `migod` yang memetakan Conversation ke keanggotaan percakapan, Room
ke `Roomkeeper::authorize` dengan mask kosong, User ke akun pemanggil sendiri atau
`Social::may_interact` dengan `Interaction::LastSeen`, dan Unknown serta Game ke penolakan
karena tidak ada yang pernah menyiarkan ke sana.

Bagian 1 tetap mencantumkan `migo-gateway`, dan itu bukan kelalaian melainkan bukti bahwa
syaratnya kurang. Crate itu benar-benar lulus `cargo build`, `cargo clippy --all-targets`,
`cargo doc`, dan `cargo test`, yaitu keempat hal yang bagian 1 minta, sementara cacat di atas
lolos melewati keempatnya tanpa satu pun berubah warna, sebab tidak ada test yang menanyakan
apakah topic yang bukan milik pemanggil ditolak. Aturan di bagian 9 nomor 2 memindahkan sebuah
item ke selesai berdasarkan test yang lulus, dan itu hanya sekuat pertanyaan yang test-nya
berani ajukan. Karena itu barisnya diberi penanda alih-alih dipindahkan: memindahkannya ke
belakang dilarang aturan nomor 3, sedangkan membiarkannya tanpa tanda akan membuat tabel
mengatakan sesuatu yang tidak benar. Penanda itu dicabut pada commit yang memasukkan perbaikan
beserta test-nya, tidak lebih awal.

## 9. Aturan yang mengikat status ini

Diambil dari migo.md section 177, karena aturannya sendiri adalah bagian dari statusnya:

1. Status WAJIB diperbarui pada commit yang sama dengan perubahan kodenya.
2. Sebuah item hanya boleh ditandai selesai bila punya test yang benar-benar dijalankan CI.
3. Ketiga blok yang namanya memuat TEST BELUM DITULIS WAJIB kosong pada saat rilis, dan sebuah
   item hanya boleh berpindah keluar dari blok itu menuju selesai, tidak pernah sebaliknya.
