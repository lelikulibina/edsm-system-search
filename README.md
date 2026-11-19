# EDSM System Search

Отдельный HTTP-сервис поиска звёздных систем Elite Dangerous по локальному каталогу EDSM.

## Статус

Реализован первый каркас MVP: SQLite + 3D R*Tree, поиск центральной системы, сферический поиск, потоковый импорт JSON-массива и HTTP API. Проверка компиляции через GitHub Actions ещё должна быть подтверждена.

## Возможности

- Поиск центра по полному имени с fallback: последовательно отбрасываются последние компоненты имени.
- При нескольких совпадениях выбирается первая система в порядке `name_norm ASC, edsm_id ASC`.
- Пространственный поиск по 3D радиусу с точной проверкой по координатам `f64`.
- Потоковый импорт gzip JSON-дампов пакетами с UPSERT.
- Отдельный HTTP API без runtime-запросов к EDSM API.

## Сборка

Требуется Rust stable (edition 2024).

```bash
cargo fmt --all
cargo test
cargo build --release
```

## Первоначальная загрузка

Скачайте полный дамп EDSM `systemsWithCoordinates.json.gz`, затем:

```bash
edsm-system-search init-db --db data/systems.db
edsm-system-search import --db data/systems.db --file /path/to/systemsWithCoordinates.json.gz --batch-size 1000
```

Импортёр читает верхнеуровневый JSON-массив потоково. В оперативной памяти находится только текущая запись и ограниченный пакет. Некорректные записи пропускаются и отражаются в итоговой статистике.

## Регулярное обновление

Скачивайте `systemsWithCoordinates7days.json.gz` ежедневно и запускайте:

```bash
edsm-system-search import --db data/systems.db --file /path/to/systemsWithCoordinates7days.json.gz --batch-size 1000
```

Повторный импорт безопасен. Записи, отсутствующие в 7-дневном дампе, не удаляются. Расписание скачивания и запуска команды настраивается оператором через cron или systemd timer.

## Запуск HTTP API

```bash
edsm-system-search serve --db data/systems.db --bind 0.0.0.0:8080 --max-radius 10000
```

Переменные окружения:
- `EDSM_DB` — путь к базе (по умолчанию `data/systems.db`)
- `EDSM_BIND` — адрес прослушивания (по умолчанию `0.0.0.0:8080`)
- `EDSM_MAX_RADIUS` — максимальный радиус в световых годах (по умолчанию 10 000)

## HTTP endpoints

### Проверка состояния

`GET /health`

Возвращает `{"status":"ok"}`, если база доступна и схема создана.

### Поиск систем

`GET /search?center=Sol&radius=2500`

Успешный ответ содержит `center`, `radius`, `count` и `systems`. В каждой найденной системе присутствуют EDSM ID, id64 (если известен), имя, координаты, дата обновления и расстояние до центра.

Ошибки:
- `400 invalid_request` — пустое имя, некорректный радиус или превышен максимум.
- `404 center_not_found` — центральная система не найдена.
- `503 database_unavailable` — база недоступна для health check.
- `500 database_error` — внутренняя ошибка базы/сервиса.

## Docker

Сборка:

```bash
docker build -t edsm-system-search .
```

Инициализация и импорт выполняются одноразовыми контейнерами с общим томом:

```bash
docker run --rm -v edsm-data:/data edsm-system-search init-db
docker run --rm -v edsm-data:/data -v "$PWD:/input:ro" edsm-system-search import --file /input/systemsWithCoordinates.json.gz
```

Запуск API:

```bash
docker run --rm -p 8080:8080 -v edsm-data:/data edsm-system-search serve
```

## Ограничения и эксплуатация

- Каталог EDSM неполон относительно всех систем игры. Отсутствие записи не доказывает отсутствие системы в Elite Dangerous.
- Полная первоначальная загрузка большого дампа может длиться долго; срок зависит от диска и процессора. Цель «пара часов» относится к разработке MVP, а не гарантированному времени импорта.
- SQLite WAL рассчитан на локальную файловую систему и процессы на одном сервере; не размещайте файл базы на NFS/сетевом диске.
- В HTTP API ограничено восемь одновременных операций SQLite, page cache настроен на 8 MiB на соединение.
- Перед обновлением/резервным копированием учитывайте файлы WAL; для согласованного бэкапа используйте SQLite backup API или остановите писателя и выполните checkpoint.
