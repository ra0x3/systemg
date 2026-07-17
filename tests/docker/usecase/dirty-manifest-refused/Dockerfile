FROM sysg-usecase-base

COPY tests/docker/usecase/dirty-manifest-refused/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/dirty-manifest-refused/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
