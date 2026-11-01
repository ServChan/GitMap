//! Числа и даты по-русски.

use chrono::{DateTime, Datelike, Local, Timelike};

const MONTHS: [&str; 12] =
    ["января", "февраля", "марта", "апреля", "мая", "июня", "июля", "августа", "сентября", "октября", "ноября", "декабря"];
const MONTHS_PLAIN: [&str; 12] =
    ["январь", "февраль", "март", "апрель", "май", "июнь", "июль", "август", "сентябрь", "октябрь", "ноябрь", "декабрь"];

fn local(time: i64) -> Option<DateTime<Local>> {
    DateTime::from_timestamp(time, 0).map(|d| d.with_timezone(&Local))
}

pub fn plural<'a>(n: usize, one: &'a str, few: &'a str, many: &'a str) -> &'a str {
    match (n % 10, n % 100) {
        (_, 11..=14) => many,
        (1, _) => one,
        (2..=4, _) => few,
        _ => many,
    }
}

pub fn count(n: usize, one: &str, few: &str, many: &str) -> String {
    format!("{} {}", number(n), plural(n, one, few, many))
}

/// 12 345 с неразрывным пробелом между разрядами.
pub fn number(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push('\u{a0}');
        }
        out.push(digit);
    }
    out
}

/// Сколько прошло: короткая подпись для столбца.
pub fn age(time: i64) -> String {
    let days = (Local::now().timestamp() - time).max(0) / 86_400;
    match days {
        0 => "сегодня".to_owned(),
        1 => "вчера".to_owned(),
        2..=13 => format!("{days} дн"),
        14..=59 => format!("{} нед", days / 7),
        60..=729 => format!("{} мес", days / 30),
        _ => {
            let years = days as usize / 365;
            format!("{years} {}", plural(years, "год", "года", "лет"))
        }
    }
}

pub fn date(time: i64) -> String {
    local(time).map_or_else(String::new, |d| {
        format!("{} {} {}, {:02}:{:02}", d.day(), MONTHS[d.month0() as usize], d.year(), d.hour(), d.minute())
    })
}

pub fn day(time: i64) -> String {
    local(time).map_or_else(String::new, |d| format!("{} {} {}", d.day(), MONTHS[d.month0() as usize], d.year()))
}

pub fn month_year(time: i64) -> String {
    local(time).map_or_else(String::new, |d| format!("{} {}", MONTHS_PLAIN[d.month0() as usize], d.year()))
}

pub fn year(time: i64) -> String {
    local(time).map_or_else(String::new, |d| d.year().to_string())
}

pub fn month(time: i64) -> String {
    local(time).map_or_else(String::new, |d| MONTHS_PLAIN[d.month0() as usize].to_owned())
}

pub fn day_of_month(time: i64) -> String {
    local(time).map_or_else(String::new, |d| d.day().to_string())
}
