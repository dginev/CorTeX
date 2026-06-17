var refreshTimer;
function startRefreshTimer() {
  var refresh_secs = localStorage.getItem("report-auto-refresh");
  if (!refresh_secs || refresh_secs == "undefined") {
    clearTimeout(refreshTimer);
  } else {
    $("input[name='refresh']").attr("checked", "checked");
    refreshTimer = setTimeout(function () { $("label.switch").replaceWith("<span>Refreshing ...</span>"); window.location.reload(true); }, refresh_secs * 1000);
  }
}

$(document).ready(function () {
  startRefreshTimer();

  var report_div = $("div#report-div");
  // The Full Corpus cells render thousands-separated counts (e.g. "3,791") via the
  // `group_thousands` filter. parseInt() is needed ONLY for the arithmetic below
  // (percentages and the completed total) — and must strip the separators first,
  // because parseInt("3,791") stops at the comma and returns 3 (which truncated
  // every count >= 1000 in this derived table; sub-1000 counts have no comma and
  // displayed correctly). For display we render the grouped number to match the
  // Full Corpus column.
  var reportCount = function (selector) {
    return parseInt($(selector).text().replace(/[^0-9]/g, ""), 10) || 0;
  };
  var grouped = function (n) { return n.toLocaleString("en-US"); };
  var pct = function (n, total) { return total > 0 ? ((100.0 * n) / total).toFixed(2) : "0.00"; };
  var queued_count = reportCount("td.queued");
  if (queued_count > 0) {
    // conversion in progress, create temporary rerun report
    var no_problem = reportCount("td.no-problem");
    var warning = reportCount("td.warning");
    var error = reportCount("td.error");
    var fatal = reportCount("td.fatal");
    var total = no_problem + warning + error + fatal;
    report_div.prepend("<br><h2>Full Corpus</h2>");

    var td_a_no_problem = $("tr.corpus-report-no-problems").find('>:first-child');
    var tr_no_problem = $('<tr class="success corpus-report-no-problems" />');
    tr_no_problem.append(td_a_no_problem.clone());
    tr_no_problem.append('<td class="right">' + pct(no_problem, total) + '%</td>');
    tr_no_problem.append('<td class="right no-problem">' + grouped(no_problem) + '</td>');

    var td_a_warning = $("tr.corpus-report-warnings").find('>:first-child');
    var tr_warning = $('<tr class="warning corpus-report-warnings" />');
    tr_warning.append(td_a_warning.clone());
    tr_warning.append('<td class="right">' + pct(warning, total) + '%</td>');
    tr_warning.append('<td class="right no-problem">' + grouped(warning) + '</td>');

    var td_a_error = $("tr.corpus-report-errors").find('>:first-child');
    var tr_error = $('<tr class="error corpus-report-errors" />');
    tr_error.append(td_a_error.clone());
    tr_error.append('<td class="right">' + pct(error, total) + '%</td>');
    tr_error.append('<td class="right no-problem">' + grouped(error) + '</td>');

    var td_a_fatal = $("tr.corpus-report-fatal").find('>:first-child');
    var tr_fatal = $('<tr class="danger corpus-report-fatal" />');
    tr_fatal.append(td_a_fatal.clone());
    tr_fatal.append('<td class="right">' + pct(fatal, total) + '%</td>');
    tr_fatal.append('<td class="right no-problem">' + grouped(fatal) + '</td>');

    var tr_completed = $('<tr class="corpus-report-completed" />');
    tr_completed.append('<td class="left">Completed</td>');
    tr_completed.append('<td class="right">100%</td>');
    tr_completed.append('<td class="right no-problem">' + grouped(total) + '</td>');


    var table = $('<table class="table"/>');
    table.append('<thead><tr><th class="left">Status</th><th class="right">Percent</th><th class="right">Tasks</th></tr></thead>');
    var tbody = $("<tbody />");
    tbody.append(tr_no_problem);
    tbody.append(tr_warning);
    tbody.append(tr_error);
    tbody.append(tr_fatal);
    tbody.append(tr_completed);
    table.append(tbody);
    report_div.prepend(table);
    report_div.prepend("<h2>Rerun Progress</h2>");
  }
  report_div.css('visibility', 'visible');

  $("input[name='refresh']").click(function () {
    var $this = $(this);
    if ($this.attr("checked")) {
      // disable auto-refresh
      $this.removeAttr("checked");
      localStorage.removeItem("report-auto-refresh");
    } else {
      // enable auto-refresh
      $this.attr("checked", "checked");
      localStorage.setItem("report-auto-refresh", 60);
    }
    startRefreshTimer();
  });
})
