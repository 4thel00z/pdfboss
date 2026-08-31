// Turns the book title into the pdfboss.dev wordmark linking back to the site.
document.addEventListener("DOMContentLoaded", function () {
  var title = document.querySelector(".menu-title");
  if (!title) {
    return;
  }
  title.textContent = "";
  var link = document.createElement("a");
  link.href = "https://pdfboss.dev/";
  link.appendChild(document.createTextNode("pdfboss"));
  var tld = document.createElement("span");
  tld.className = "wordmark-tld";
  tld.textContent = ".dev";
  link.appendChild(tld);
  title.appendChild(link);
  var section = document.createElement("span");
  section.className = "wordmark-section";
  section.textContent = "Docs";
  title.appendChild(section);
});
