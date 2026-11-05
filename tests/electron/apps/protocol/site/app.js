console.log('href', location.href);
console.log('color', getComputedStyle(document.querySelector('h1')).color);
fetch('data.json')
  .then((response) => response.json())
  .then((data) => console.log('data', JSON.stringify(data)));
