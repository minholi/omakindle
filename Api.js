.pragma library

function parseJson(text, fallback) {
  try {
    return JSON.parse(String(text || ""))
  } catch (error) {
    return fallback
  }
}

function redact(value) {
  var text = String(value || "").replace(/\s+/g, " ").trim()
  if (text.length > 300) text = text.substring(0, 297) + "..."
  return text
}

function formatPercent(value) {
  var number = Number(value)
  if (!isFinite(number) || number <= 0) return ""
  if (number >= 99.5) return "100%"
  if (number < 1) return number.toFixed(1) + "%"
  return Math.round(number) + "%"
}

function barLabel(book, showProgress) {
  if (!book) return ""
  var title = String(book.title || "").trim()
  var percent = showProgress ? formatPercent(book.percentageRead) : ""
  return percent === "" ? title : title + "  " + percent
}

function bookByAsin(books, asin) {
  var target = String(asin || "")
  for (var index = 0; index < (books || []).length; index++)
    if (String(books[index].asin) === target) return books[index]
  return null
}

function matchesQuery(book, query) {
  var needle = String(query || "").trim().toLowerCase()
  if (needle === "") return true
  var authors = (book && book.authors ? book.authors : []).join(" ")
  var haystack = String(book && book.title || "") + " " + authors
  return haystack.toLowerCase().indexOf(needle) !== -1
}

function normalizeRegion(value) {
  var region = String(value || "us").trim().toLowerCase()
  return /^[a-z.]{2,6}$/.test(region) ? region : "us"
}
